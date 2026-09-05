//! Runtime support the derive macro expands into.
//!
//! Nothing here is intended to be named by hand; it is `#[doc(hidden)]` in the
//! re-export so it does not clutter the crate's public surface. It is a normal
//! module rather than macro-generated code so that all of it is unit-testable.

use vaco_core::Duration;

use crate::{ArrayDesc, OptError, OptRangeDisplay};

pub use crate::desc::{HasSchema, OptEnumConsts};
pub use crate::{
    ConstDesc, ConstValue, OptBase, OptFlags, OptId, OptKind, OptValue, OptValueKind, OptionDesc,
    Options, ParseCtx, Schema, SerCtx, parse_flag_bits, serialize_flag_bits,
};

/// Build the display-only range pair. `const` so it can sit in a `static`.
#[must_use]
pub const fn range_display(min: f64, max: f64) -> OptRangeDisplay {
    OptRangeDisplay { min, max }
}

/// Build an array modifier. `const` so it can sit in a `static`.
#[must_use]
pub const fn array(sep: char, min_len: u32, max_len: u32) -> ArrayDesc {
    ArrayDesc {
        sep,
        min_len,
        max_len,
    }
}

/// A lossy widening used only to build the error message for a failed range
/// check. The check itself never goes through `f64`.
pub trait BoundF64: Copy {
    fn bound_f64(self) -> f64;
}

macro_rules! impl_bound {
    ($($t:ty),*) => {
        $(impl BoundF64 for $t {
            #[allow(
                clippy::cast_lossless,
                clippy::cast_precision_loss,
                reason = "one uniform widening for every bound type; the value is only \
                          ever used to build an error message"
            )]
            fn bound_f64(self) -> f64 { self as f64 }
        })*
    };
}
impl_bound!(
    i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64, usize, isize
);

/// A field whose value can be checked against a typed inclusive range.
///
/// The check runs against the *typed* value, not against the `f64` pair in the
/// descriptor: `FFmpeg` stores `min`/`max` as `double`, which silently loses
/// precision above 2^53 and mis-validates `int64` options such as byte limits.
pub trait RangeCheckable {
    type Bound: Copy + PartialOrd + BoundF64;

    /// # Errors
    ///
    /// [`OptError::OutOfRange`] when the value falls outside `lo..=hi`.
    fn check(&self, lo: Self::Bound, hi: Self::Bound, name: &str) -> Result<(), OptError>;
}

fn out_of_range<B: BoundF64>(v: B, lo: B, hi: B, name: &str) -> OptError {
    OptError::OutOfRange {
        name: name.to_owned(),
        value: v.bound_f64(),
        min: lo.bound_f64(),
        max: hi.bound_f64(),
    }
}

macro_rules! impl_range_scalar {
    ($($t:ty),*) => {
        $(impl RangeCheckable for $t {
            type Bound = $t;
            fn check(&self, lo: $t, hi: $t, name: &str) -> Result<(), OptError> {
                if *self < lo || *self > hi {
                    Err(out_of_range(*self, lo, hi, name))
                } else {
                    Ok(())
                }
            }
        })*
    };
}
impl_range_scalar!(i8, i16, i32, i64, u8, u16, u32, u64, f32, f64, usize, isize);

impl RangeCheckable for Duration {
    type Bound = i64;
    fn check(&self, lo: i64, hi: i64, name: &str) -> Result<(), OptError> {
        if *self < Duration::from_micros(lo) || *self > Duration::from_micros(hi) {
            Err(OptError::OutOfRange {
                name: name.to_owned(),
                value: self.as_secs_f64() * 1_000_000.0,
                min: lo.bound_f64(),
                max: hi.bound_f64(),
            })
        } else {
            Ok(())
        }
    }
}

impl<T: RangeCheckable> RangeCheckable for Option<T> {
    type Bound = T::Bound;
    fn check(&self, lo: Self::Bound, hi: Self::Bound, name: &str) -> Result<(), OptError> {
        match self {
            Some(v) => v.check(lo, hi, name),
            None => Ok(()),
        }
    }
}

impl<T: RangeCheckable> RangeCheckable for Vec<T> {
    type Bound = T::Bound;
    fn check(&self, lo: Self::Bound, hi: Self::Bound, name: &str) -> Result<(), OptError> {
        for v in self {
            v.check(lo, hi, name)?;
        }
        Ok(())
    }
}

/// The entry point the derive emits, one call per ranged option.
///
/// # Errors
///
/// [`OptError::OutOfRange`] when the value falls outside `lo..=hi`.
pub fn check_range<T: RangeCheckable>(
    v: &T,
    lo: T::Bound,
    hi: T::Bound,
    name: &str,
) -> Result<(), OptError> {
    v.check(lo, hi, name)
}
