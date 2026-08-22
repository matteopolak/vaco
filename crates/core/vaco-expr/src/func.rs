//! The builtin function table.
//!
//! Names and arities were established by probing every candidate name at every
//! arity from 0 to 4 against the reference; the table below is exactly the set
//! that came back as "known". `log2`, `log10`, `exp2`, `cbrt`, `sign`, `fmod`,
//! `sinc`, `asinh`, `bitxor` and two dozen other plausible names are *not*
//! functions in this language, and must stay rejected.

/// Every builtin function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs, reason = "one variant per documented function name")]
pub enum Func {
    Abs,
    Acos,
    Asin,
    Atan,
    Atan2,
    Between,
    BitAnd,
    BitOr,
    Ceil,
    Clip,
    Cos,
    Cosh,
    Eq,
    Exp,
    Floor,
    Gauss,
    Gcd,
    Gt,
    Gte,
    Hypot,
    If,
    IfNot,
    IsInf,
    IsNan,
    Ld,
    Lerp,
    Log,
    Lt,
    Lte,
    Max,
    Min,
    Mod,
    Not,
    Pow,
    Print,
    Random,
    RandomI,
    Root,
    Round,
    Sgn,
    Sin,
    Sinh,
    Sqrt,
    Squish,
    St,
    Tan,
    Tanh,
    Taylor,
    Time,
    Trunc,
    While,
    /// A function supplied by the caller through [`crate::Bindings`].
    Extern(u16),
}

/// Name, function, minimum arity, maximum arity.
///
/// Order matters only in that the first prefix match wins (see
/// [`crate::lex::strmatch`]); no builtin name is a prefix of another under
/// that rule, so the table is listed alphabetically for readability.
pub(crate) const BUILTINS: &[(&str, Func, u8, u8)] = &[
    ("abs", Func::Abs, 1, 1),
    ("acos", Func::Acos, 1, 1),
    ("asin", Func::Asin, 1, 1),
    ("atan2", Func::Atan2, 2, 2),
    ("atan", Func::Atan, 1, 1),
    ("between", Func::Between, 3, 3),
    ("bitand", Func::BitAnd, 2, 2),
    ("bitor", Func::BitOr, 2, 2),
    ("ceil", Func::Ceil, 1, 1),
    ("clip", Func::Clip, 3, 3),
    ("cosh", Func::Cosh, 1, 1),
    ("cos", Func::Cos, 1, 1),
    ("eq", Func::Eq, 2, 2),
    ("exp", Func::Exp, 1, 1),
    ("floor", Func::Floor, 1, 1),
    ("gauss", Func::Gauss, 1, 1),
    ("gcd", Func::Gcd, 2, 2),
    ("gte", Func::Gte, 2, 2),
    ("gt", Func::Gt, 2, 2),
    ("hypot", Func::Hypot, 2, 2),
    ("ifnot", Func::IfNot, 2, 3),
    ("if", Func::If, 2, 3),
    ("isinf", Func::IsInf, 1, 1),
    ("isnan", Func::IsNan, 1, 1),
    ("ld", Func::Ld, 1, 1),
    ("lerp", Func::Lerp, 3, 3),
    ("log", Func::Log, 1, 1),
    ("lte", Func::Lte, 2, 2),
    ("lt", Func::Lt, 2, 2),
    ("max", Func::Max, 2, 2),
    ("min", Func::Min, 2, 2),
    ("mod", Func::Mod, 2, 2),
    ("not", Func::Not, 1, 1),
    ("pow", Func::Pow, 2, 2),
    ("print", Func::Print, 1, 3),
    ("randomi", Func::RandomI, 3, 3),
    ("random", Func::Random, 1, 1),
    ("root", Func::Root, 2, 2),
    ("round", Func::Round, 1, 1),
    ("sgn", Func::Sgn, 1, 1),
    ("sinh", Func::Sinh, 1, 1),
    ("sin", Func::Sin, 1, 1),
    ("sqrt", Func::Sqrt, 1, 1),
    ("squish", Func::Squish, 1, 1),
    ("st", Func::St, 2, 2),
    ("tanh", Func::Tanh, 1, 1),
    ("tan", Func::Tan, 1, 1),
    ("taylor", Func::Taylor, 2, 3),
    ("time", Func::Time, 1, 1),
    ("trunc", Func::Trunc, 1, 1),
    ("while", Func::While, 2, 2),
];

impl Func {
    /// The canonical name, or `None` for a caller-supplied function.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        BUILTINS
            .iter()
            .find(|(_, f, _, _)| *f == self)
            .map(|(n, _, _, _)| *n)
    }

    /// Whether this function reads or writes an `ld`/`st` register.
    ///
    /// Callers that evaluate in a hot loop use this to decide whether the
    /// register file has to survive between frames.
    #[must_use]
    pub const fn touches_registers(self) -> bool {
        matches!(
            self,
            Self::Ld | Self::St | Self::Random | Self::RandomI | Self::Root | Self::Taylor
        )
    }
}
