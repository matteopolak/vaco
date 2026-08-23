//! Stream disposition flags, in one place.
//!
//! # Why it lives here
//!
//! `vaco-cli-core` and `vaco-format-core` each defined a `Disposition`, with
//! the same nineteen flags at the same nineteen bits and the same names. One
//! concept twice (D19). Neither depends on the other, so the shared home has to
//! sit below both.
//!
//! # Merging them found a bug
//!
//! The two disagreed about **case**. `vaco-cli-core::by_name` was
//! case-sensitive; `vaco-format-core::from_cli_name` used
//! `eq_ignore_ascii_case`. Measured against ffmpeg 8.1:
//!
//! ```text
//! -disposition:v:0 default   accepted
//! -disposition:v:0 DEFAULT   Undefined constant or missing '(' in 'DEFAULT'
//! -disposition:v:0 Default   Undefined constant or missing '(' in 'Default'
//! ```
//!
//! Case-sensitive, and the error message says why: the reference resolves these
//! through its expression evaluator's named-constant table, which is exact. The
//! case-insensitive version accepted input the reference rejects — a divergence
//! that only existed because there were two answers to one question.
//!
//! This is the argument for D19 stated compactly: duplication is not merely
//! wasteful, it is where two behaviours hide behind one name.

/// Which of the reference's `AV_DISPOSITION_*` flags a stream carries.
///
/// Hand-rolled rather than `bitflags`: `vaco-core` has no dependencies and the
/// whole type is thirty lines. The bit numbers are interface facts (D9) — they
/// are what `-disposition` parses and what `ffprobe -show_streams` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Disposition(u32);

macro_rules! dispositions {
    ($($bit:literal => $konst:ident, $name:literal;)*) => {
        impl Disposition {
            $(
                #[doc = concat!("`", $name, "`")]
                pub const $konst: Self = Self(1 << $bit);
            )*

            /// Every flag paired with its name, in bit order.
            ///
            /// Bit order is also **output order**: `ffprobe` prints the
            /// `DISPOSITION` block in this sequence, so reordering this table
            /// reorders the output.
            pub const ALL: &'static [(Self, &'static str)] = &[
                $((Self(1 << $bit), $name),)*
            ];
        }
    };
}

dispositions! {
     0 => DEFAULT,           "default";
     1 => DUB,               "dub";
     2 => ORIGINAL,          "original";
     3 => COMMENT,           "comment";
     4 => LYRICS,            "lyrics";
     5 => KARAOKE,           "karaoke";
     6 => FORCED,            "forced";
     7 => HEARING_IMPAIRED,  "hearing_impaired";
     8 => VISUAL_IMPAIRED,   "visual_impaired";
     9 => CLEAN_EFFECTS,     "clean_effects";
    10 => ATTACHED_PIC,      "attached_pic";
    11 => TIMED_THUMBNAILS,  "timed_thumbnails";
    12 => NON_DIEGETIC,      "non_diegetic";
    13 => CAPTIONS,          "captions";
    14 => DESCRIPTIONS,      "descriptions";
    15 => METADATA,          "metadata";
    16 => DEPENDENT,         "dependent";
    17 => STILL_IMAGE,       "still_image";
    18 => MULTILAYER,        "multilayer";
}

impl Disposition {
    /// No flags set.
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Exactly these bits, including any this build does not name.
    ///
    /// Preserving unknown bits is deliberate: a container may state a flag from
    /// a newer revision, and dropping it silently would make a round-trip lossy
    /// in a way nothing reports. Use [`Disposition::from_bits_truncate`] when
    /// the caller genuinely wants only the known set.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// These bits, with anything this build does not name masked off.
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        // 19 flags at bits 0..19.
        Self(bits & ((1 << 19) - 1))
    }

    /// Every named flag set.
    #[must_use]
    pub const fn all() -> Self {
        Self((1 << 19) - 1)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every bit of `other` is set here.
    ///
    /// An empty `other` always matches, which is what makes `disp:0` select
    /// every stream.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Set or clear `flag`, the shape a demuxer wants while reading a header.
    pub const fn set(&mut self, flag: Self, on: bool) {
        if on {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    /// Resolve one flag by the name the tool prints and `-disposition` accepts.
    ///
    /// **Case-sensitive**, measured: the reference resolves these through its
    /// expression evaluator's named-constant table, so `DEFAULT` is
    /// `Undefined constant or missing '('`, not a synonym for `default`.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, n)| *n == name)
            .map(|&(flag, _)| flag)
    }

    /// The names of the set bits, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::ALL
            .iter()
            .filter(move |(f, _)| self.contains(*f))
            .map(|&(_, n)| n)
    }

    /// Every flag paired with whether it is set, in output order.
    ///
    /// What `ffprobe`'s `DISPOSITION` block needs: it prints all nineteen with
    /// a 0 or 1, not only the set ones.
    pub fn fields(self) -> impl Iterator<Item = (&'static str, bool)> {
        Self::ALL.iter().map(move |&(d, n)| (n, self.contains(d)))
    }
}

impl core::ops::BitOr for Disposition {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for Disposition {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nineteen_flags_at_consecutive_bits() {
        assert_eq!(Disposition::ALL.len(), 19);
        for (i, &(flag, _)) in Disposition::ALL.iter().enumerate() {
            assert_eq!(flag.bits(), 1 << i, "bit {i}");
        }
    }

    #[test]
    fn truncate_drops_bits_this_build_does_not_name_and_from_bits_keeps_them() {
        let unknown = 1_u32 << 31;
        assert_eq!(Disposition::from_bits_truncate(unknown).bits(), 0);
        assert_eq!(Disposition::from_bits(unknown).bits(), unknown);
        assert_eq!(
            Disposition::all(),
            Disposition::ALL
                .iter()
                .fold(Disposition::NONE, |a, &(f, _)| a.union(f))
        );
    }

    #[test]
    fn name_lookup_is_case_sensitive() {
        // Measured against ffmpeg 8.1: `-disposition:v:0 DEFAULT` fails with
        // "Undefined constant or missing '(' in 'DEFAULT'". One of the two
        // pre-merge implementations accepted it.
        assert_eq!(Disposition::by_name("default"), Some(Disposition::DEFAULT));
        assert_eq!(Disposition::by_name("DEFAULT"), None);
        assert_eq!(Disposition::by_name("Default"), None);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        // What makes `disp:0` select every stream rather than none.
        assert!(Disposition::NONE.contains(Disposition::NONE));
        assert!(Disposition::DEFAULT.contains(Disposition::NONE));
    }

    #[test]
    fn set_clears_as_well_as_sets() {
        let mut d = Disposition::NONE;
        d.set(Disposition::FORCED, true);
        assert!(d.contains(Disposition::FORCED));
        d.set(Disposition::FORCED, false);
        assert!(d.is_empty());
    }

    #[test]
    fn fields_reports_every_flag_and_names_only_the_set_ones() {
        let d = Disposition::DEFAULT | Disposition::FORCED;
        assert_eq!(d.fields().count(), 19);
        assert_eq!(d.names().collect::<Vec<_>>(), ["default", "forced"]);
    }
}
