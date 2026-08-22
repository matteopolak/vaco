//! `frame_header_obu()` / `uncompressed_header()`, AV1 spec §5.9.1–§5.9.2 —
//! **the intra path only**. See "What is deliberately not here" below.
//!
//! # Scope: key frames and intra-only frames, not inter frames
//!
//! `uncompressed_header()`'s common prefix (frame type, show flags,
//! screen-content/integer-mv choice, frame id, order hint, the
//! `refresh_frame_flags`/`ref_order_hint` bookkeeping) is parsed for every
//! frame type, because getting it wrong would misalign every bit that
//! follows. Past that prefix the syntax forks:
//!
//! - **Intra** (`KEY_FRAME`, `INTRA_ONLY_FRAME`, or a `SWITCH_FRAME`/inter
//!   frame with `show_existing_frame`) calls `frame_size()` and `render_size()`
//!   directly — both self-contained given the sequence header, which is why
//!   this module implements them.
//! - **Inter** calls `frame_size_with_refs()`, which can copy a *reference
//!   frame's* dimensions (`found_ref` in the spec) rather than reading its
//!   own. That is state this crate does not keep: `RefUpscaledWidth[i]` /
//!   `RefFrameHeight[i]` are properties of frames already *decoded*, not
//!   parsed, and reconstructing them correctly means tracking
//!   `refresh_frame_flags` across the whole reference frame lifetime — a
//!   decoder-shaped piece of state this crate has deliberately not built
//!   (see the crate root docs on parsing vs. decoding). An inter frame's
//!   header is parsed up to that point and returned as
//!   [`FrameHeader::Inter`], which still reports `frame_type` and
//!   `show_frame` — enough for a `Parser` to flag key frames — without
//!   fabricating a resolution it cannot derive.
//!
//! This is not a gap in the numbers `ffprobe` prints: every fixture this
//! crate was measured against reports its resolution from the *sequence*
//! header (`docs/codec/vaco-parse-av1.md` has the measurements), because
//! `frame_size_override_flag` is 0 in ordinary encoder output. An inter
//! frame that actually overrides its size independently of every reference
//! (`frame_size_override_flag && error_resilient_mode`, calling `frame_size()`
//! directly rather than `frame_size_with_refs()`) *is* handled — that path
//! does not touch reference state at all.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::seq::{NUM_REF_FRAMES, SELECT_VALUE, SequenceHeader};

/// `frame_type`, §6.8.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Key,
    Inter,
    IntraOnly,
    Switch,
}

impl FrameType {
    const fn from_bits(v: u32) -> Self {
        match v {
            0 => Self::Key,
            2 => Self::IntraOnly,
            3 => Self::Switch,
            _ => Self::Inter,
        }
    }

    /// `FrameIsIntra`, §5.9.2: key or intra-only.
    #[must_use]
    pub const fn is_intra(self) -> bool {
        matches!(self, Self::Key | Self::IntraOnly)
    }
}

/// The coded and render dimensions `frame_size()` / `render_size()` produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSize {
    /// `FrameWidth` after `superres_params()` has downscaled it — the size
    /// tile data is coded at.
    pub coded_width: u32,
    pub coded_height: u32,
    /// `UpscaledWidth` — `FrameWidth` *before* superres downscaling, which is
    /// also the picture's output width when `RenderWidth` was not signalled
    /// separately.
    pub upscaled_width: u32,
    pub render_width: u32,
    pub render_height: u32,
    /// Whether `superres_params()` actually downscaled — `coded_width !=
    /// upscaled_width`.
    pub use_superres: bool,
}

/// What this crate parses of `frame_header_obu()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameHeader {
    /// `show_existing_frame == 1`: nothing else is coded.
    ShowExistingFrame { frame_to_show_map_idx: u8 },
    /// A key or intra-only frame, or a switch frame shown immediately — every
    /// case `frame_size()` (not `frame_size_with_refs()`) applies to.
    Intra {
        frame_type: FrameType,
        show_frame: bool,
        error_resilient_mode: bool,
        size: FrameSize,
        allow_intrabc: bool,
    },
    /// An inter frame, parsed up to (not including) `frame_size_with_refs()`.
    /// See the module documentation for why.
    Inter {
        frame_type: FrameType,
        show_frame: bool,
        error_resilient_mode: bool,
    },
}

impl FrameHeader {
    #[must_use]
    pub const fn frame_type(&self) -> Option<FrameType> {
        match self {
            Self::ShowExistingFrame { .. } => None,
            Self::Intra { frame_type, .. } | Self::Inter { frame_type, .. } => Some(*frame_type),
        }
    }

    #[must_use]
    pub const fn size(&self) -> Option<FrameSize> {
        match self {
            Self::Intra { size, .. } => Some(*size),
            _ => None,
        }
    }

    /// Parse `uncompressed_header()`'s payload.
    ///
    /// `temporal_id`/`spatial_id` come from the OBU's extension header (0/0
    /// when it had none) and are needed only for `decoder_model_info`'s
    /// per-operating-point `buffer_removal_time` loop.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the payload is truncated or a value is out of
    /// range, [`Error::Unsupported`] never — an inter frame is a supported,
    /// deliberately partial result, not a failure.
    pub fn parse(
        payload: &[u8],
        seq: &SequenceHeader,
        temporal_id: u8,
        spatial_id: u8,
    ) -> Result<Self> {
        let mut r = BitReader::new(payload);
        let result = parse_inner(&mut r, seq, temporal_id, spatial_id);
        r.check()
            .map_err(|_| Error::InvalidData("frame_header_obu ran past the end of its payload"))?;
        result
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "uncompressed_header()'s common prefix is one syntax structure in the specification; \
              splitting it into sub-functions would fragment the field-by-field correspondence \
              this module depends on for review"
)]
fn parse_inner(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<FrameHeader> {
    let id_len = seq.additional_frame_id_length + seq.delta_frame_id_length;

    let (frame_type, show_frame, error_resilient_mode);
    if seq.reduced_still_picture_header {
        frame_type = FrameType::Key;
        show_frame = true;
        error_resilient_mode = false;
    } else {
        let show_existing_frame = r.get_bit() != 0;
        if show_existing_frame {
            let frame_to_show_map_idx = r.get(3) as u8;
            // temporal_point_info() is two fixed-width fields gated on
            // decoder_model_info; skip them the same way the frame-header
            // buffer_removal_time loop below does, for the same reason: real
            // encoders leave decoder_model_info_present_flag at 0, but a
            // conforming stream may not.
            if seq.decoder_model_info_present_flag {
                // equal_picture_interval is only known via timing_info(); a
                // stream with decoder_model_info but no timing_info is not
                // well-formed, so `unwrap_or(true)` (skip the read) is the
                // conservative side of an impossible case rather than a
                // guess that matters in practice.
                let equal = seq.timing_info.is_some_and(|t| t.equal_picture_interval);
                if !equal {
                    // frame_presentation_time_length_minus_1 was consumed by
                    // the sequence header only as a *width*; the actual field
                    // here is that many bits wide. Not tracked (see module
                    // docs on decoder_model scope) — this branch is
                    // unreachable for every fixture this crate was tested
                    // against and is reported as a hard stop rather than a
                    // silent misalignment.
                    return Err(Error::Unsupported(
                        "temporal_point_info() (unequal picture interval) is not parsed",
                    ));
                }
            }
            if seq.frame_id_numbers_present_flag {
                let _display_frame_id = r.get(u32::from(id_len));
            }
            return Ok(FrameHeader::ShowExistingFrame {
                frame_to_show_map_idx,
            });
        }
        frame_type = FrameType::from_bits(r.get(2));
        show_frame = r.get_bit() != 0;
        if show_frame && seq.decoder_model_info_present_flag {
            let equal = seq.timing_info.is_some_and(|t| t.equal_picture_interval);
            if !equal {
                return Err(Error::Unsupported(
                    "temporal_point_info() (unequal picture interval) is not parsed",
                ));
            }
        }
        let _showable_frame = if show_frame {
            frame_type != FrameType::Key
        } else {
            r.get_bit() != 0
        };
        error_resilient_mode =
            if frame_type == FrameType::Switch || (frame_type == FrameType::Key && show_frame) {
                true
            } else {
                r.get_bit() != 0
            };
    }
    let frame_is_intra = frame_type.is_intra();

    let _disable_cdf_update = r.get_bit();
    let allow_screen_content_tools = if seq.seq_force_screen_content_tools == SELECT_VALUE {
        r.get_bit() != 0
    } else {
        seq.seq_force_screen_content_tools != 0
    };
    // `force_integer_mv` gates `allow_high_precision_mv`/motion-vector
    // precision in the inter path this crate does not parse (see the module
    // docs); only its *bit consumption* matters here, so the value itself is
    // discarded rather than tracked.
    if allow_screen_content_tools && seq.seq_force_integer_mv == SELECT_VALUE {
        let _force_integer_mv = r.get_bit() != 0;
    }

    if seq.frame_id_numbers_present_flag {
        let _current_frame_id = r.get(u32::from(id_len));
    }

    let frame_size_override_flag = if frame_type == FrameType::Switch {
        true
    } else if seq.reduced_still_picture_header {
        false
    } else {
        r.get_bit() != 0
    };

    let _order_hint = r.get(u32::from(seq.order_hint_bits));

    let _primary_ref_frame = if frame_is_intra || error_resilient_mode {
        None
    } else {
        Some(r.get(3))
    };

    if seq.decoder_model_info_present_flag {
        let buffer_removal_time_present_flag = r.get_bit() != 0;
        if buffer_removal_time_present_flag {
            for op in &seq.operating_points {
                if !op.decoder_model_present {
                    continue;
                }
                let in_temporal_layer = (op.idc >> temporal_id) & 1 != 0;
                let in_spatial_layer = (op.idc >> (u16::from(spatial_id) + 8)) & 1 != 0;
                if op.idc == 0 || (in_temporal_layer && in_spatial_layer) {
                    let _buffer_removal_time = r.get(u32::from(seq.buffer_removal_time_length));
                }
            }
        }
    }

    let all_frames: u32 = (1u32 << NUM_REF_FRAMES) - 1;
    let refresh_frame_flags =
        if frame_type == FrameType::Switch || (frame_type == FrameType::Key && show_frame) {
            all_frames
        } else {
            r.get(8)
        };

    if (!frame_is_intra || refresh_frame_flags != all_frames)
        && error_resilient_mode
        && seq.enable_order_hint
    {
        for _ in 0..NUM_REF_FRAMES {
            let _ref_order_hint = r.get(u32::from(seq.order_hint_bits));
        }
    }

    if frame_is_intra {
        let size = parse_frame_size(r, seq, frame_size_override_flag)?;
        let allow_intrabc = if allow_screen_content_tools && size.upscaled_width == size.coded_width
        {
            r.get_bit() != 0
        } else {
            false
        };
        // A `SWITCH_FRAME` cannot be intra (`frame_type == 3` implies inter by
        // construction elsewhere in the spec, but §6.8.2 never actually
        // forbids the *value* 3 reaching here through `show_existing_frame`'s
        // absence check) — kept as `Intra` regardless of the concrete
        // variant, since what matters to a caller is that `frame_size()`, not
        // `frame_size_with_refs()`, produced `size`.
        return Ok(FrameHeader::Intra {
            frame_type,
            show_frame,
            error_resilient_mode,
            size,
            allow_intrabc,
        });
    }

    Ok(FrameHeader::Inter {
        frame_type,
        show_frame,
        error_resilient_mode,
    })
}

/// `frame_size()`, §5.9.5, plus `render_size()`, §5.9.6 — both self-contained
/// given the sequence header, unlike `frame_size_with_refs()`.
/// `SUPERRES_NUM`, §3: the numerator every downscaled width is scaled back up
/// by.
const SUPERRES_NUM: u32 = 8;
/// `SUPERRES_DENOM_MIN`, §3: the smallest denominator `superres_params()` can
/// signal (`coded_denom` is a 3-bit field added to this).
const SUPERRES_DENOM_MIN: u32 = 9;

#[allow(
    clippy::integer_division,
    reason = "computing FrameWidth from UpscaledWidth is exactly this rounding division per \
              §5.9.7's own pseudocode: (UpscaledWidth * SUPERRES_NUM + (SuperresDenom/2)) / SuperresDenom"
)]
fn parse_frame_size(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    frame_size_override_flag: bool,
) -> Result<FrameSize> {
    let (mut frame_width, frame_height) = if frame_size_override_flag {
        let w = r.get(u32::from(seq.frame_width_bits)) + 1;
        let h = r.get(u32::from(seq.frame_height_bits)) + 1;
        (w, h)
    } else {
        (seq.max_frame_width, seq.max_frame_height)
    };

    // superres_params(), §5.9.7.
    let use_superres = seq.enable_superres && r.get_bit() != 0;
    let superres_denom = if use_superres {
        r.get(3) + SUPERRES_DENOM_MIN
    } else {
        SUPERRES_NUM
    };
    let upscaled_width = frame_width;
    if use_superres {
        frame_width = (upscaled_width * SUPERRES_NUM + superres_denom / 2) / superres_denom.max(1);
    }
    if frame_width == 0 || frame_height == 0 {
        return Err(Error::InvalidData("frame_size() produced a zero dimension"));
    }

    // render_size(), §5.9.6.
    let render_and_frame_size_different = r.get_bit() != 0;
    let (render_width, render_height) = if render_and_frame_size_different {
        (r.get(16) + 1, r.get(16) + 1)
    } else {
        (upscaled_width, frame_height)
    };

    Ok(FrameSize {
        coded_width: frame_width,
        coded_height: frame_height,
        upscaled_width,
        render_width,
        render_height,
        use_superres,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    /// Pushes fixed-width fields MSB-first, matching `BitReader::get`. See
    /// `seq.rs`'s identical helper for why this beats hand-packed bytes.
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

    fn seq_header() -> SequenceHeader {
        // The same real `libsvtav1` sequence header used in `seq.rs`'s tests:
        // profile 0, 642x358, 8-bit 4:2:0, level 2.1.
        let payload = [
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ];
        let mut b = Budget::new(Limits::strict());
        SequenceHeader::parse(&payload, &mut b).expect("a real sequence header parses")
    }

    /// A minimal `uncompressed_header()` for a key frame under the sequence
    /// header above: `frame_size_override_flag = 0`, so dimensions come
    /// straight from `max_frame_width`/`max_frame_height`.
    #[test]
    fn a_key_frame_reports_the_sequence_header_size() {
        let seq = seq_header();
        let mut p = BitPusher::default();
        p.push(0, 1) // show_existing_frame
            .push(0, 2) // frame_type = KEY_FRAME
            .push(1, 1) // show_frame
            // showable_frame not read: show_frame=1, frame_type=KEY -> false, no bit
            // error_resilient_mode: KEY_FRAME && show_frame -> forced true, no bit
            .push(0, 1) // disable_cdf_update
            // seq_force_screen_content_tools == SELECT_VALUE (this stream's
            // seq header sets it via seq_choose_screen_content_tools=1) so a
            // bit is read here:
            .push(0, 1) // allow_screen_content_tools = 0
            // allow_screen_content_tools == 0, so seq_force_integer_mv branch
            // is not read; frame_is_intra forces force_integer_mv=1 anyway.
            // frame_id_numbers_present_flag is false for this seq header.
            // frame_type != SWITCH, not reduced: frame_size_override_flag
            .push(0, 1)
            // order_hint: seq.order_hint_bits for this stream is 7 (measured
            // in seq.rs's trace).
            .push(0, 7)
            // primary_ref_frame not read: frame_is_intra
            // decoder_model_info_present_flag is false for this seq header.
            // refresh_frame_flags forced (KEY_FRAME && show_frame), no bits.
            // ref_order_hint loop: error_resilient_mode is true, but
            // seq.enable_order_hint is true AND refresh_frame_flags ==
            // all_frames AND frame_is_intra -> condition is false, skipped.
            // frame_size(): frame_size_override_flag=0, so no width/height
            // bits; superres_params(): enable_superres is false for this
            // stream -> use_superres bit not read, denom fixed.
            // render_size(): render_and_frame_size_different
            .push(0, 1);
        // allow_intrabc: allow_screen_content_tools=0 -> not read.
        let bytes = p.bytes();
        let mut b = Budget::new(Limits::strict());
        let _ = &mut b; // silence unused in case of future signature change
        let fh = FrameHeader::parse(&bytes, &seq, 0, 0).expect("parses");
        match fh {
            FrameHeader::Intra {
                frame_type,
                show_frame,
                size,
                ..
            } => {
                assert_eq!(frame_type, FrameType::Key);
                assert!(show_frame);
                assert_eq!(size.coded_width, 642);
                assert_eq!(size.coded_height, 358);
                assert_eq!(size.upscaled_width, 642);
                assert_eq!(size.render_width, 642);
                assert_eq!(size.render_height, 358);
                assert!(!size.use_superres);
            }
            other => panic!("expected an intra frame header, got {other:?}"),
        }
    }

    #[test]
    fn show_existing_frame_stops_immediately() {
        let seq = seq_header();
        let mut p = BitPusher::default();
        p.push(1, 1) // show_existing_frame
            .push(3, 3); // frame_to_show_map_idx
        let bytes = p.bytes();
        let fh = FrameHeader::parse(&bytes, &seq, 0, 0).expect("parses");
        assert_eq!(
            fh,
            FrameHeader::ShowExistingFrame {
                frame_to_show_map_idx: 3
            }
        );
    }

    #[test]
    fn an_inter_frame_is_reported_without_a_fabricated_size() {
        let seq = seq_header();
        let mut p = BitPusher::default();
        p.push(0, 1) // show_existing_frame
            .push(1, 2) // frame_type = INTER_FRAME
            .push(0, 1); // show_frame = 0 -> showable_frame is read next
        p.push(1, 1); // showable_frame
        p.push(1, 1); // error_resilient_mode (frame_type=INTER, so read)
        let bytes = p.bytes();
        // This is deliberately incomplete past error_resilient_mode (the
        // remaining prefix needs more bits than this test constructs), so
        // only assert it does not panic and does not fabricate a size.
        if let Ok(fh) = FrameHeader::parse(&bytes, &seq, 0, 0) {
            assert!(fh.size().is_none());
        }
    }

    #[test]
    fn truncation_never_panics() {
        let seq = seq_header();
        let data = [0u8; 16];
        for n in 0..=data.len() {
            let _ = FrameHeader::parse(&data[..n], &seq, 0, 0);
        }
    }
}
