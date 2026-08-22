//! `profile_tier_level()`, ITU-T H.265 §7.3.3 — the structure HEVC differs from
//! H.264 in most, and the one that is easiest to get subtly wrong.
//!
//! # Why it is easy to get wrong
//!
//! H.264 spends three bytes on this: `profile_idc`, a constraint byte, and
//! `level_idc`. HEVC spends **twelve** on the general layer alone, and then a
//! variable number more on sub-layers:
//!
//! ```text
//!   general_profile_space              u(2)  ─┐
//!   general_tier_flag                  u(1)   │ 40 bits
//!   general_profile_idc                u(5)   │
//!   general_profile_compatibility_flag u(32) ─┘
//!   ── 43 bits of constraint flags, whose NAMES depend on the profile ──
//!   general_progressive_source_flag    u(1)  ─┐
//!   general_interlaced_source_flag     u(1)   │
//!   general_non_packed_constraint_flag u(1)   │ 48 bits
//!   general_frame_only_constraint_flag u(1)   │
//!   ...43 bits...                             │
//!   general_inbld_flag                 u(1)  ─┘
//!   general_level_idc                  u(8)
//!   ── then, per sub-layer, two presence flags, then padding to 8, then
//!      the whole 88-bit block again for each sub-layer that has one ──
//! ```
//!
//! Three specific traps:
//!
//! 1. **The 43 bits are always 43 bits.** §7.3.3 splits them differently
//!    depending on `general_profile_idc` and on the compatibility flags — the
//!    range-extension profiles name nine of them, `Main 10` names one, and
//!    every other profile calls all 43 reserved — but the *total never
//!    changes*. A parser that branches on the profile to decide how many bits
//!    to read has invented a bug that only fires on streams it was not tested
//!    against. This module reads the 43 bits as one value and names them
//!    afterwards.
//! 2. **The sub-layer padding is conditional.** The
//!    `reserved_zero_2bits[i]` run for `i` in `maxNumSubLayersMinus1..8`
//!    is present **only if `maxNumSubLayersMinus1 > 0`**. A parser that always
//!    reads it desynchronises every single-layer stream, which is almost all of
//!    them.
//! 3. **A sub-layer's profile and level are independently present.** The two
//!    flags are read for every sub-layer *first*, in one run, and only then are
//!    the bodies read. Interleaving them desynchronises any stream with more
//!    than one sub-layer.

use vaco_codec_golomb::BoundedGolomb;
use vaco_core::Result;

use crate::util::MAX_SUB_LAYERS;

/// The profile and tier half of a `profile_tier_level()` layer — everything
/// except `level_idc`.
///
/// The same 88 bits appear for the general layer and for each sub-layer that
/// declares one, so it is one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct ProfileTier {
    /// `general_profile_space` / `sub_layer_profile_space`, 0..=3. Anything but
    /// 0 means the profile numbering is not the one Annex A defines.
    pub profile_space: u8,
    /// `general_tier_flag`: false is Main tier, true is High tier. The tier
    /// changes what a given `level_idc` permits, so a level number without a
    /// tier is only half an answer.
    pub tier_flag: bool,
    /// `general_profile_idc`, 0..=31.
    pub profile_idc: u8,
    /// The 32 `general_profile_compatibility_flag[j]` bits, `j` in bit
    /// `31 - j` — so flag 0 is the most significant bit, matching the order
    /// they are read in.
    pub compatibility_flags: u32,
    /// `general_progressive_source_flag`.
    pub progressive_source: bool,
    /// `general_interlaced_source_flag`.
    pub interlaced_source: bool,
    /// `general_non_packed_constraint_flag`.
    pub non_packed_constraint: bool,
    /// `general_frame_only_constraint_flag`.
    pub frame_only_constraint: bool,
    /// The 43 bits between `frame_only_constraint_flag` and `inbld`, most
    /// significant first — the first bit read is bit 42.
    ///
    /// Stored raw because §7.3.3 gives them different *names* depending on the
    /// profile but always the same *count*. [`ProfileTier::constraint`] applies
    /// the naming.
    pub constraint_bits: u64,
    /// `general_inbld_flag`, or the `general_reserved_zero_bit` that occupies
    /// the same position for profiles that have no INBLD.
    pub inbld: bool,
}

/// One of the nine named constraint flags §7.3.3 defines for the
/// range-extension profile family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Constraint {
    /// `general_max_12bit_constraint_flag`.
    Max12Bit,
    /// `general_max_10bit_constraint_flag`.
    Max10Bit,
    /// `general_max_8bit_constraint_flag`.
    Max8Bit,
    /// `general_max_422chroma_constraint_flag`.
    Max422Chroma,
    /// `general_max_420chroma_constraint_flag`.
    Max420Chroma,
    /// `general_max_monochrome_constraint_flag`.
    MaxMonochrome,
    /// `general_intra_constraint_flag`.
    Intra,
    /// `general_one_picture_only_constraint_flag`.
    OnePictureOnly,
    /// `general_lower_bit_rate_constraint_flag`.
    LowerBitRate,
    /// `general_max_14bit_constraint_flag`, present only for the high-throughput
    /// and screen-content families.
    Max14Bit,
}

impl ProfileTier {
    /// Whether `general_profile_compatibility_flag[j]` is set.
    ///
    /// `j` above 31 is false rather than a panic: the field is exactly 32 bits
    /// and a caller asking beyond it has a bug, not the stream.
    #[must_use]
    pub const fn compatible_with(self, j: u8) -> bool {
        if j >= 32 {
            return false;
        }
        self.compatibility_flags & (0x8000_0000u32 >> j) != 0
    }

    /// Whether the profile is `idc`, either by `general_profile_idc` or by the
    /// compatibility flag — §A.3's own phrasing of "conforms to profile *i*".
    #[must_use]
    pub const fn claims_profile(self, idc: u8) -> bool {
        self.profile_idc == idc || self.compatible_with(idc)
    }

    /// The **effective** profile: `general_profile_idc` when it says anything,
    /// otherwise the lowest set compatibility flag.
    ///
    /// `// D17:` measured, not assumed. `general_profile_idc` was patched to
    /// each value 0..=11 with the compatibility flags cleared, and then to 0
    /// with exactly one compatibility flag set, in a 640x360 stream from
    /// `x265`; `ffprobe 8.1 -f hevc -show_entries stream=profile` was read back
    /// for all 24 rows. With `profile_idc` non-zero the compatibility flags are
    /// ignored; with `profile_idc == 0` the answer is the lowest set flag, and
    /// with none set it prints `0`.
    ///
    /// The specification does not describe a precedence at all — it says a
    /// stream *conforms to* every profile it claims — so a tool has to choose
    /// one to print, and this is the choice the reference made.
    #[must_use]
    pub const fn effective_profile_idc(self) -> u8 {
        if self.profile_idc != 0 {
            return self.profile_idc;
        }
        if self.compatibility_flags == 0 {
            return 0;
        }
        self.compatibility_flags.leading_zeros() as u8
    }

    /// The 48 bits `hvcC` stores as `general_constraint_indicator_flags`,
    /// ISO/IEC 14496-15 §8.3.3.1.2 — the four named flags, the 43 middle bits
    /// and `inbld`, in that order, right-aligned in a `u64`.
    ///
    /// This is why [`constraint_bits`](Self::constraint_bits) is stored raw:
    /// the configuration record carries exactly this block, so a parser that
    /// decomposed it by profile would have to put it back together to write an
    /// `hvcC`.
    #[must_use]
    pub const fn constraint_indicator_flags(self) -> u64 {
        ((self.progressive_source as u64) << 47)
            | ((self.interlaced_source as u64) << 46)
            | ((self.non_packed_constraint as u64) << 45)
            | ((self.frame_only_constraint as u64) << 44)
            | ((self.constraint_bits & 0x7FF_FFFF_FFFF) << 1)
            | (self.inbld as u64)
    }

    /// Rebuild a [`ProfileTier`]'s flag block from the 48 bits an `hvcC`
    /// carries — the inverse of
    /// [`constraint_indicator_flags`](Self::constraint_indicator_flags).
    #[must_use]
    pub const fn with_constraint_indicator_flags(mut self, bits: u64) -> Self {
        self.progressive_source = (bits >> 47) & 1 != 0;
        self.interlaced_source = (bits >> 46) & 1 != 0;
        self.non_packed_constraint = (bits >> 45) & 1 != 0;
        self.frame_only_constraint = (bits >> 44) & 1 != 0;
        self.constraint_bits = (bits >> 1) & 0x7FF_FFFF_FFFF;
        self.inbld = bits & 1 != 0;
        self
    }

    /// Whether the profile puts this layer in §7.3.3's range-extension branch,
    /// where the first nine of the 43 bits are named constraint flags.
    ///
    /// The condition is `general_profile_idc` in 4..=11 **or** any of
    /// `general_profile_compatibility_flag[4..=11]`. Both halves matter: a
    /// stream may declare `profile_idc = 4` with no compatibility flags, or
    /// `profile_idc = 0` with flag 4 set, and the 43 bits are named the same
    /// way in either case.
    #[must_use]
    pub const fn has_named_constraints(self) -> bool {
        let mut j = 4u8;
        if self.profile_idc >= 4 && self.profile_idc <= 11 {
            return true;
        }
        while j <= 11 {
            if self.compatible_with(j) {
                return true;
            }
            j += 1;
        }
        false
    }

    /// Whether §7.3.3 gives this layer a `general_max_14bit_constraint_flag` in
    /// place of one of the reserved bits — the high-throughput (5, 11) and
    /// screen-content (9, 10) families.
    #[must_use]
    const fn has_max_14bit(self) -> bool {
        self.claims_profile(5)
            || self.claims_profile(9)
            || self.claims_profile(10)
            || self.claims_profile(11)
    }

    /// A named constraint flag, or `None` when this profile does not name that
    /// bit — in which case it is a reserved bit whose value carries no meaning.
    ///
    /// Returning `None` rather than `false` matters: "the flag is 0" and "there
    /// is no such flag in this profile" are different facts, and only the first
    /// says anything about the stream.
    #[must_use]
    pub const fn constraint(self, which: Constraint) -> Option<bool> {
        // Bit 42 is the first of the 43, read first.
        const fn bit(bits: u64, n: u32) -> bool {
            bits & (1u64 << (42 - n)) != 0
        }
        let named = self.has_named_constraints();
        let index = match which {
            Constraint::Max12Bit if named => 0,
            Constraint::Max10Bit if named => 1,
            Constraint::Max8Bit if named => 2,
            Constraint::Max422Chroma if named => 3,
            Constraint::Max420Chroma if named => 4,
            Constraint::MaxMonochrome if named => 5,
            Constraint::Intra if named => 6,
            Constraint::OnePictureOnly if named => 7,
            Constraint::LowerBitRate if named => 8,
            Constraint::Max14Bit if named && self.has_max_14bit() => 9,
            // §7.3.3's Main-10 branch: seven reserved bits, then the single
            // `general_one_picture_only_constraint_flag`, then 35 reserved.
            Constraint::OnePictureOnly if self.profile_idc == 2 || self.compatible_with(2) => 7,
            _ => return None,
        };
        Some(bit(self.constraint_bits, index))
    }
}

/// One sub-layer's entry in a `profile_tier_level()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubLayerPtl {
    /// `sub_layer_profile_present_flag[i]` and what it introduced.
    pub profile: Option<ProfileTier>,
    /// `sub_layer_level_present_flag[i]` and what it introduced.
    pub level_idc: Option<u8>,
}

/// `profile_tier_level()`, §7.3.3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileTierLevel {
    /// The general layer, present only when the caller passed
    /// `profilePresentFlag = 1` — which every call in this crate does, so it is
    /// `None` only for the multi-layer extension syntax we do not parse.
    pub general: Option<ProfileTier>,
    /// `general_level_idc`. Always present.
    ///
    /// Thirty times the level number: level 4.1 is 123, level 2.1 is 63. That
    /// is what `ffprobe` prints as `level`, unscaled.
    pub general_level_idc: u8,
    /// One entry per sub-layer below the highest, so `len()` is
    /// `maxNumSubLayersMinus1`.
    pub sub_layers: Vec<SubLayerPtl>,
}

impl ProfileTierLevel {
    /// Read a `profile_tier_level()`.
    ///
    /// `max_num_sub_layers_minus1` is the caller's — the VPS's or the SPS's —
    /// and is clamped to 7 because the field it comes from is `u(3)`.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`](vaco_core::Error::UnexpectedEof) on truncation,
    /// or a budget error.
    pub fn parse(
        g: &mut BoundedGolomb<'_, '_, '_>,
        profile_present: bool,
        max_num_sub_layers_minus1: u32,
    ) -> Result<Self> {
        let sub_layers_minus1 = max_num_sub_layers_minus1.min(MAX_SUB_LAYERS - 1);
        let general = if profile_present {
            Some(read_profile_tier(g)?)
        } else {
            None
        };
        let general_level_idc = g.u(8)? as u8;

        // Both presence flags for every sub-layer, in one run — §7.3.3 reads
        // them all before any body. Interleaving is the classic bug here.
        let mut present = [(false, false); MAX_SUB_LAYERS as usize];
        for i in 0..sub_layers_minus1 as usize {
            let p = g.u(1)? != 0;
            let l = g.u(1)? != 0;
            if let Some(slot) = present.get_mut(i) {
                *slot = (p, l);
            }
        }
        // Padding to eight entries, present ONLY when there is more than one
        // sub-layer. A parser that reads it unconditionally desynchronises
        // every single-layer stream, which is nearly every stream.
        if sub_layers_minus1 > 0 {
            for _ in sub_layers_minus1..MAX_SUB_LAYERS {
                g.u(2)?;
            }
        }

        let mut sub_layers = Vec::new();
        for i in 0..sub_layers_minus1 as usize {
            let (p, l) = present.get(i).copied().unwrap_or((false, false));
            sub_layers.push(SubLayerPtl {
                profile: if p { Some(read_profile_tier(g)?) } else { None },
                level_idc: if l { Some(g.u(8)? as u8) } else { None },
            });
        }

        Ok(Self {
            general,
            general_level_idc,
            sub_layers,
        })
    }
}

/// The 88-bit profile-and-constraint block, §7.3.3, shared by the general layer
/// and every sub-layer.
fn read_profile_tier(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<ProfileTier> {
    let profile_space = g.u(2)? as u8;
    let tier_flag = g.u(1)? != 0;
    let profile_idc = g.u(5)? as u8;
    let compatibility_flags = g.u(32)?;
    let progressive_source = g.u(1)? != 0;
    let interlaced_source = g.u(1)? != 0;
    let non_packed_constraint = g.u(1)? != 0;
    let frame_only_constraint = g.u(1)? != 0;
    // The 43 bits, as one value. `u()` reads at most 32 bits at a time.
    let hi = u64::from(g.u(32)?);
    let lo = u64::from(g.u(11)?);
    let constraint_bits = (hi << 11) | lo;
    let inbld = g.u(1)? != 0;
    Ok(ProfileTier {
        profile_space,
        tier_flag,
        profile_idc,
        compatibility_flags,
        progressive_source,
        interlaced_source,
        non_packed_constraint,
        frame_only_constraint,
        constraint_bits,
        inbld,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_bitstream::BitReader;
    use vaco_limits::{Budget, Limits};

    fn parse(bytes: &[u8], sub_layers_minus1: u32) -> (ProfileTierLevel, u64) {
        let mut reader = BitReader::new(bytes);
        let mut budget = Budget::new(Limits::strict());
        let mut g = BoundedGolomb::new(&mut reader, &mut budget);
        let ptl = ProfileTierLevel::parse(&mut g, true, sub_layers_minus1).expect("parses");
        (ptl, reader.bit_pos())
    }

    /// The twelve bytes an `x265` Main-profile SPS carries, lifted verbatim from
    /// `sd.265` (bytes 3..15 of the SPS RBSP).
    const MAIN_PTL: &[u8] = &[
        0x01, 0x60, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f,
    ];

    #[test]
    fn the_general_layer_is_exactly_ninety_six_bits() {
        let (ptl, pos) = parse(MAIN_PTL, 0);
        assert_eq!(pos, 96, "88 bits of profile plus 8 of level");
        let g = ptl.general.expect("profile present");
        assert_eq!(g.profile_space, 0);
        assert!(!g.tier_flag);
        assert_eq!(g.profile_idc, 1);
        // x265 sets compatibility flags 1 and 2.
        assert!(g.compatible_with(1));
        assert!(g.compatible_with(2));
        assert!(!g.compatible_with(0));
        assert!(!g.compatible_with(3));
        assert!(g.progressive_source);
        assert!(!g.interlaced_source);
        assert!(!g.non_packed_constraint);
        assert!(g.frame_only_constraint);
        assert!(!g.inbld);
        // level 2.1 == 63.
        assert_eq!(ptl.general_level_idc, 63);
        assert!(ptl.sub_layers.is_empty());
    }

    /// The 4:4:4 stream's `profile_idc = 4` puts the 43 bits in the named
    /// branch; `x265` sets `max_12bit`, `max_10bit`, `max_8bit` and
    /// `lower_bit_rate`.
    #[test]
    fn the_range_extension_constraints_are_named() {
        // From `p444.265`: profile_idc 4, compat flag 4, constraints 0x9e08...
        let bytes = &[
            0x04, 0x08, 0x00, 0x00, 0x00, 0x9e, 0x08, 0x00, 0x00, 0x00, 0x00, 0x3f,
        ];
        let (ptl, pos) = parse(bytes, 0);
        assert_eq!(pos, 96);
        let g = ptl.general.expect("profile present");
        assert_eq!(g.profile_idc, 4);
        assert!(g.has_named_constraints());
        assert_eq!(g.constraint(Constraint::Max12Bit), Some(true));
        assert_eq!(g.constraint(Constraint::Max10Bit), Some(true));
        assert_eq!(g.constraint(Constraint::Max8Bit), Some(true));
        assert_eq!(g.constraint(Constraint::Max422Chroma), Some(false));
        assert_eq!(g.constraint(Constraint::Max420Chroma), Some(false));
        assert_eq!(g.constraint(Constraint::MaxMonochrome), Some(false));
        assert_eq!(g.constraint(Constraint::Intra), Some(false));
        assert_eq!(g.constraint(Constraint::OnePictureOnly), Some(false));
        assert_eq!(g.constraint(Constraint::LowerBitRate), Some(true));
        // Not a high-throughput or screen-content profile, so no 14-bit flag.
        assert_eq!(g.constraint(Constraint::Max14Bit), None);
    }

    /// The monochrome stream sets `general_max_monochrome_constraint_flag`,
    /// which is how a caller can tell RExt-monochrome from RExt-4:4:4 without
    /// reading the SPS body.
    #[test]
    fn monochrome_is_visible_in_the_constraint_flags() {
        // From `mono.265`.
        let bytes = &[
            0x04, 0x08, 0x00, 0x00, 0x00, 0x9f, 0xc8, 0x00, 0x00, 0x00, 0x00, 0x3f,
        ];
        let (ptl, _) = parse(bytes, 0);
        let g = ptl.general.expect("profile present");
        assert_eq!(g.constraint(Constraint::MaxMonochrome), Some(true));
        assert_eq!(g.constraint(Constraint::Max420Chroma), Some(true));
        assert_eq!(g.constraint(Constraint::Max422Chroma), Some(true));
    }

    /// A single-layer stream reads **no** `reserved_zero_2bits` padding, and a
    /// multi-layer one reads all of it. Getting this backwards is trap 2.
    #[test]
    fn the_sub_layer_padding_is_conditional() {
        let mut bytes = MAIN_PTL.to_vec();
        bytes.extend_from_slice(&[0; 40]);
        // One sub-layer: 96 bits and nothing more.
        let (_, pos0) = parse(&bytes, 0);
        assert_eq!(pos0, 96);
        // Two sub-layers: 96, then 2 presence bits, then 7 * 2 bits of padding
        // (i is 1..8), then a body only if a presence flag was set — and the
        // zero bytes here set none.
        let (ptl1, pos1) = parse(&bytes, 1);
        assert_eq!(pos1, 96 + 2 + 14);
        assert_eq!(ptl1.sub_layers.len(), 1);
        assert!(ptl1.sub_layers[0].profile.is_none());
        assert!(ptl1.sub_layers[0].level_idc.is_none());
    }

    /// A sub-layer that declares a profile costs another 88 bits, and one that
    /// declares a level another 8 — and the presence flags for *every*
    /// sub-layer are read before any body.
    #[test]
    fn sub_layer_bodies_follow_all_of_the_presence_flags() {
        let mut bytes = MAIN_PTL.to_vec();
        // Two sub-layers below the top: presence flags 11 for the first, 00 for
        // the second, then 6 * 2 bits of padding (i = 2..8) — 16 bits exactly.
        bytes.push(0b1100_0000);
        bytes.push(0);
        bytes.extend_from_slice(MAIN_PTL); // the first sub-layer's 88 + 8
        bytes.extend_from_slice(&[0; 8]);
        let (ptl, pos) = parse(&bytes, 2);
        assert_eq!(ptl.sub_layers.len(), 2);
        assert!(ptl.sub_layers[0].profile.is_some());
        assert_eq!(ptl.sub_layers[0].level_idc, Some(63));
        assert!(ptl.sub_layers[1].profile.is_none());
        assert_eq!(pos, 96 + 4 + 12 + 96);
    }

    /// `hvcC` carries the 48 constraint bits verbatim, so the accessor and its
    /// inverse must be exact.
    #[test]
    fn the_hvcc_constraint_block_round_trips() {
        let (ptl, _) = parse(MAIN_PTL, 0);
        let g = ptl.general.expect("profile present");
        let bits = g.constraint_indicator_flags();
        // From the real `hvcC`: 90 00 00 00 00 00.
        assert_eq!(bits, 0x9000_0000_0000);
        let back = ProfileTier::default().with_constraint_indicator_flags(bits);
        assert_eq!(back.progressive_source, g.progressive_source);
        assert_eq!(back.interlaced_source, g.interlaced_source);
        assert_eq!(back.non_packed_constraint, g.non_packed_constraint);
        assert_eq!(back.frame_only_constraint, g.frame_only_constraint);
        assert_eq!(back.constraint_bits, g.constraint_bits);
        assert_eq!(back.inbld, g.inbld);
    }

    /// The effective-profile rule, exactly as probed.
    #[test]
    fn the_effective_profile_falls_back_to_the_lowest_compatibility_flag() {
        let mut pt = ProfileTier {
            profile_idc: 1,
            compatibility_flags: 0x6000_0000, // flags 1 and 2
            ..ProfileTier::default()
        };
        assert_eq!(pt.effective_profile_idc(), 1, "idc wins when non-zero");
        pt.profile_idc = 0;
        assert_eq!(pt.effective_profile_idc(), 1, "lowest set flag");
        pt.compatibility_flags = 0x0800_0000; // flag 4 only
        assert_eq!(pt.effective_profile_idc(), 4);
        pt.compatibility_flags = 0;
        assert_eq!(pt.effective_profile_idc(), 0, "nothing claimed at all");
        pt.compatibility_flags = 1; // flag 31
        assert_eq!(pt.effective_profile_idc(), 31);
    }

    #[test]
    fn a_truncated_ptl_is_an_error_not_a_panic() {
        for n in 0..MAIN_PTL.len() {
            let mut reader = BitReader::new(&MAIN_PTL[..n]);
            let mut budget = Budget::new(Limits::strict());
            let mut g = BoundedGolomb::new(&mut reader, &mut budget);
            let _ = ProfileTierLevel::parse(&mut g, true, 7);
        }
    }
}
