//! Per-context option flags, and the `-h full` flag column.

/// Bit flags classifying which tool and which context an option applies to.
///
/// Reproduced from the option-system inventory (research §5) because they are
/// the interface fact behind `-h full`'s flag column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OptFlags(u32);

macro_rules! flag_consts {
    ($($name:ident = $bit:expr, $col:expr, $lower:expr;)*) => {
        impl OptFlags {
            $(pub const $name: Self = Self(1 << $bit);)*

            /// Every flag that has a column in [`OptFlags::column`], in column order.
            const COLUMNS: [(Self, u8); 11] = [$((Self(1 << $bit), $col)),*];

            /// Look a flag up by its lower-case attribute spelling.
            ///
            /// This is the mapping `#[opt(flags(audio, runtime))]` uses, and the
            /// derive macro keeps its own copy of the same table; this one exists
            /// so runtime-built schemas and tests can use the same names.
            #[must_use]
            pub fn from_attr_name(s: &str) -> Option<Self> {
                match s {
                    $($lower => Some(Self::$name),)*
                    "param" => Some(Self::PARAM),
                    _ => None,
                }
            }
        }
    };
}

// The column order is E D F V A S X R B T P. It is pinned by the snapshot test
// in `tests/snapshot.rs` and is intended to be validated against the reference
// tool by the `-h full` differential harness (plan 11 §6.9), which lives in
// `vaco-cli-core` and is not this crate's to build.
flag_consts! {
    ENCODING     = 0,  b'E', "encoding";
    DECODING     = 1,  b'D', "decoding";
    FILTERING    = 2,  b'F', "filtering";
    VIDEO        = 3,  b'V', "video";
    AUDIO        = 4,  b'A', "audio";
    SUBTITLE     = 5,  b'S', "subtitle";
    EXPORT       = 6,  b'X', "export";
    READONLY     = 7,  b'R', "readonly";
    BSF          = 8,  b'B', "bsf";
    RUNTIME      = 9,  b'T', "runtime";
    DEPRECATED   = 10, b'P', "deprecated";
}

impl OptFlags {
    /// Not printed in the flag column; it controls whether child objects
    /// contribute their named constants to this option's unit.
    pub const CHILD_CONSTS: Self = Self(1 << 11);

    /// The `param` shorthand accepted by `#[opt(flags(param))]`.
    pub const PARAM: Self = Self(Self::ENCODING.0 | Self::DECODING.0);

    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every bit of `o` is present in `self`. An empty `o` is always
    /// contained, matching the usual bitflag convention.
    #[must_use]
    pub const fn contains(self, o: Self) -> bool {
        self.0 & o.0 == o.0
    }

    /// True when any bit is shared.
    #[must_use]
    pub const fn intersects(self, o: Self) -> bool {
        self.0 & o.0 != 0
    }

    #[must_use]
    pub const fn union(self, o: Self) -> Self {
        Self(self.0 | o.0)
    }

    #[must_use]
    pub const fn difference(self, o: Self) -> Self {
        Self(self.0 & !o.0)
    }

    /// The `-h full` flag column, e.g. `..FV.....T.`.
    #[must_use]
    pub fn column(self) -> [u8; 11] {
        let mut out = [b'.'; 11];
        for (slot, (flag, ch)) in out.iter_mut().zip(Self::COLUMNS) {
            if self.contains(flag) {
                *slot = ch;
            }
        }
        out
    }

    /// [`OptFlags::column`] as a `String`, for help rendering and snapshots.
    #[must_use]
    pub fn column_string(self) -> String {
        self.column().iter().map(|&b| char::from(b)).collect()
    }
}

impl core::ops::BitOr for OptFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for OptFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
