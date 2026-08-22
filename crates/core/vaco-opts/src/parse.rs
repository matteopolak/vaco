//! Re-export of `vaco_core::parse`, plus the two newtypes that stay here.
//!
//! This module briefly carried a full local implementation, written when
//! `vaco-core` was frozen but unimplemented. `vaco-core` now owns the parsers.
//!
//! `VideoRate` and `Binary` do not move there: they exist only because a type can
//! carry exactly one `OptValue` impl, and `OptBase::Rational` / `OptBase::VideoRate`
//! are distinct option types with distinct grammars. That is an option-system
//! concern with no meaning in `vaco-core`.

use vaco_core::Rational;
pub use vaco_core::parse::*;

/// A newtype over [`Rational`] rather than a bare `Rational`, because
/// `OptBase::Rational` and `OptBase::VideoRate` are different option types with
/// different grammars and a type can carry only one `OptValue` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoRate(pub Rational);

/// Raw bytes as an option value.
///
/// A newtype rather than a bare `Vec<u8>` so it does not collide with the
/// blanket array impl for `Vec<T>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Binary(pub Vec<u8>);
