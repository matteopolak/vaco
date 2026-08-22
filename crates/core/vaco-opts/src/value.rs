//! [`OptValue`]: the trait that makes a Rust type usable as an option field.
//!
//! Type erasure runs through `&mut dyn OptValue` rather than through byte
//! offsets. `FFmpeg` computes `(char *)obj + offset` and casts to whatever the
//! table claims; that is the one pattern safe Rust cannot have, and the
//! substitute — a generated `match` returning a trait object — costs one jump
//! table and makes the type mismatch impossible.
//!
//! Layer-1 crates implement this trait for their own types, which is how
//! `vaco-opts` names `OptBase::PixelFmt` without ever naming `PixelFormat`.

use core::any::Any;
use core::fmt::Write as _;

use vaco_core::{Duration, Rational};

use crate::parse::{Binary, Rgba, VideoRate};
use crate::{ArrayDesc, ConstDesc, Dict, OptBase, OptError, OptRangeDisplay, escape, parse};

/// Everything a value needs in order to parse itself: the option's name (for
/// error messages), its unit's named constants, its display range and its
/// array modifier.
///
/// Plan 11 §6.3 omits `name`; without it an `OptValue` impl cannot build an
/// [`OptError`], so it is added here.
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseCtx<'a> {
    pub name: &'a str,
    pub consts: &'a [ConstDesc],
    pub unit: Option<&'a str>,
    pub range: Option<OptRangeDisplay>,
    pub array: Option<ArrayDesc>,
}

/// The serialisation counterpart of [`ParseCtx`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SerCtx<'a> {
    pub name: &'a str,
    pub consts: &'a [ConstDesc],
    pub unit: Option<&'a str>,
    pub array: Option<ArrayDesc>,
}

impl<'a> ParseCtx<'a> {
    /// A context with no unit and no array, for the typed unit tests and for
    /// values parsed outside a schema.
    #[must_use]
    pub fn bare(name: &'a str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }
}

impl<'a> SerCtx<'a> {
    #[must_use]
    pub fn bare(name: &'a str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }
}

/// The static base kind of a value type.
///
/// A separate trait from [`OptValue`] rather than an associated const on it, as
/// plan 11 §6.3 writes it: an associated const makes a trait non-dyn-compatible,
/// and the `where Self: Sized` escape hatch the plan uses is still unstable
/// ("generic const items", rust#113521). `#[derive(Options)]` reads `BASE` from
/// here to fill in [`crate::OptionDesc::kind`].
pub trait OptValueKind {
    const BASE: OptBase;
}

/// A type that can back an option field.
pub trait OptValue: Any + Send + Sync + core::fmt::Debug {
    /// The base kind, through a trait object.
    fn base(&self) -> OptBase;

    /// Parse `s` into `self`.
    ///
    /// `ctx` supplies the named constants of this option's unit, so enums,
    /// flags and plain integers with named values all go through one path.
    ///
    /// # Errors
    ///
    /// Returns [`OptError::InvalidValue`], [`OptError::UnknownConst`],
    /// [`OptError::ArrayLen`] or [`OptError::Escape`] when `s` is not a legal
    /// value for this type.
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError>;

    /// Append the canonical string form. Must round-trip through
    /// [`OptValue::parse_into`].
    fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>);

    /// Numeric view, for `query_ranges` and help rendering. `None` for
    /// non-numeric kinds. Never used for the authoritative range check.
    fn as_f64(&self) -> Option<f64> {
        None
    }

    fn eq_dyn(&self, other: &dyn OptValue) -> bool;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Snapshot, so a failed parse can be rolled back and the "no partial
    /// application" invariant holds.
    fn clone_box(&self) -> Box<dyn OptValue>;

    /// Restore from a snapshot. `false` when `src` is a different type.
    fn assign_from(&mut self, src: &dyn OptValue) -> bool;
}

/// Emit the mechanical half of an [`OptValue`] impl.
///
/// Layer-1 crates implementing `OptValue` for their own format enums write
/// `vaco_opts::impl_opt_value_common!(PixelFormat);` inside the impl and then
/// only supply `BASE`, `parse_into` and `serialize`. Requires
/// `Self: Clone + PartialEq`.
#[macro_export]
macro_rules! impl_opt_value_common {
    ($t:ty) => {
        fn base(&self) -> $crate::OptBase {
            <$t as $crate::OptValueKind>::BASE
        }
        fn eq_dyn(&self, other: &dyn $crate::OptValue) -> bool {
            other.as_any().downcast_ref::<$t>() == ::core::option::Option::Some(self)
        }
        fn as_any(&self) -> &dyn ::core::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn ::core::any::Any {
            self
        }
        fn clone_box(&self) -> ::std::boxed::Box<dyn $crate::OptValue> {
            ::std::boxed::Box::new(::core::clone::Clone::clone(self))
        }
        fn assign_from(&mut self, src: &dyn $crate::OptValue) -> bool {
            match src.as_any().downcast_ref::<$t>() {
                ::core::option::Option::Some(v) => {
                    ::core::clone::Clone::clone_from(self, v);
                    true
                }
                ::core::option::Option::None => false,
            }
        }
    };
}

// ------------------------------------------------------------------ helpers

pub(crate) fn lookup_const<'a>(ctx: &ParseCtx<'a>, name: &str) -> Option<&'a ConstDesc> {
    ctx.consts.iter().find(|c| c.name == name)
}

fn const_name_for_int(consts: &[ConstDesc], v: i64) -> Option<&'static str> {
    consts
        .iter()
        .find(|c| c.value.as_i64() == Some(v))
        .map(|c| c.name)
}

fn const_name_for_f64(consts: &[ConstDesc], v: f64) -> Option<&'static str> {
    consts
        .iter()
        .find(|c| (c.value.as_f64() - v).abs() == 0.0)
        .map(|c| c.name)
}

/// Decimal or `0x`-prefixed hex, with an optional sign.
#[must_use]
pub fn parse_integer(s: &str) -> Option<i128> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() {
        return None;
    }
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if body.bytes().all(|b| b.is_ascii_digit()) {
        body.parse::<i128>().ok()?
    } else {
        return None;
    };
    Some(if neg { -v } else { v })
}

fn unknown(ctx: &ParseCtx<'_>, s: &str) -> OptError {
    if ctx.consts.is_empty() {
        OptError::invalid(ctx.name, s)
    } else {
        OptError::UnknownConst {
            name: ctx.name.to_owned(),
            value: s.to_owned(),
        }
    }
}

// ------------------------------------------------------------------ integers

macro_rules! impl_int {
    ($t:ty, $base:expr) => {
        impl OptValueKind for $t {
            const BASE: OptBase = $base;
        }

        impl OptValue for $t {
            fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
                let t = s.trim();
                if let Some(v) = parse_integer(t) {
                    let v = <$t>::try_from(v).map_err(|_| OptError::invalid(ctx.name, s))?;
                    *self = v;
                    return Ok(());
                }
                if let Some(c) = lookup_const(ctx, t) {
                    let v = c
                        .value
                        .as_i64()
                        .and_then(|v| <$t>::try_from(v).ok())
                        .ok_or_else(|| OptError::invalid(ctx.name, s))?;
                    *self = v;
                    return Ok(());
                }
                Err(unknown(ctx, s))
            }

            fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>) {
                let widened = i64::try_from(*self).ok();
                if let Some(n) = widened.and_then(|v| const_name_for_int(ctx.consts, v)) {
                    out.push_str(n);
                    return;
                }
                out.push_str(&self.to_string());
            }

            #[allow(
                clippy::cast_lossless,
                clippy::cast_precision_loss,
                reason = "one uniform widening across all four integer bases; the f64 view is \
                          display-only and never used for the authoritative range check"
            )]
            fn as_f64(&self) -> Option<f64> {
                Some(*self as f64)
            }

            $crate::impl_opt_value_common!($t);
        }
    };
}

impl_int!(i32, OptBase::Int);
impl_int!(i64, OptBase::Int64);
impl_int!(u32, OptBase::UInt);
impl_int!(u64, OptBase::UInt64);

// -------------------------------------------------------------------- floats

macro_rules! impl_float {
    ($t:ty, $base:expr) => {
        impl OptValueKind for $t {
            const BASE: OptBase = $base;
        }

        impl OptValue for $t {
            fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
                let t = s.trim();
                if let Ok(v) = t.parse::<$t>() {
                    *self = v;
                    return Ok(());
                }
                if let Some(c) = lookup_const(ctx, t) {
                    *self = c.value.as_f64() as $t;
                    return Ok(());
                }
                Err(unknown(ctx, s))
            }

            fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>) {
                if let Some(n) = const_name_for_f64(ctx.consts, f64::from(*self)) {
                    out.push_str(n);
                    return;
                }
                out.push_str(&self.to_string());
            }

            fn as_f64(&self) -> Option<f64> {
                Some(f64::from(*self))
            }

            $crate::impl_opt_value_common!($t);
        }
    };
}

impl_float!(f32, OptBase::Float);
impl_float!(f64, OptBase::Double);

// --------------------------------------------------------------------- bool

impl OptValueKind for bool {
    const BASE: OptBase = OptBase::Bool;
}

impl OptValue for bool {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::boolean(s.trim()) {
            Some(v) => {
                *self = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(if *self { "true" } else { "false" });
    }

    fn as_f64(&self) -> Option<f64> {
        Some(if *self { 1.0 } else { 0.0 })
    }

    impl_opt_value_common!(bool);
}

// ------------------------------------------------------------------- string

impl OptValueKind for String {
    const BASE: OptBase = OptBase::String;
}

impl OptValue for String {
    fn parse_into(&mut self, s: &str, _ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        self.clear();
        self.push_str(s);
        Ok(())
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(self);
    }

    impl_opt_value_common!(String);
}

// ------------------------------------------------------------------- binary

impl OptValueKind for Binary {
    const BASE: OptBase = OptBase::Binary;
}

impl OptValue for Binary {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::binary(s.trim()) {
            Some(v) => {
                self.0 = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(&parse::format_binary(&self.0));
    }

    impl_opt_value_common!(Binary);
}

// --------------------------------------------------------------------- dict

impl OptValueKind for Dict {
    const BASE: OptBase = OptBase::Dict;
}

impl OptValue for Dict {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        let mut d = Dict::new();
        d.parse_string(s, "=", ":", crate::DictFlags::exact())
            .map_err(|e| OptError::Escape {
                name: ctx.name.to_owned(),
                detail: e.to_string(),
            })?;
        *self = d;
        Ok(())
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(&self.to_string_with('=', ':'));
    }

    impl_opt_value_common!(Dict);
}

// --------------------------------------------------------------- image size

impl OptValueKind for (u32, u32) {
    const BASE: OptBase = OptBase::ImageSize;
}

impl OptValue for (u32, u32) {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::image_size(s.trim()) {
            Some(v) => {
                *self = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        let _ = write!(out, "{}x{}", self.0, self.1);
    }

    impl_opt_value_common!((u32, u32));
}

// ----------------------------------------------------------------- rational

impl OptValueKind for Rational {
    const BASE: OptBase = OptBase::Rational;
}

impl OptValue for Rational {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::rational(s.trim()) {
            Some(v) => {
                *self = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        let _ = write!(out, "{}/{}", self.num, self.den);
    }

    fn as_f64(&self) -> Option<f64> {
        Some(self.to_f64())
    }

    impl_opt_value_common!(Rational);
}

impl OptValueKind for VideoRate {
    const BASE: OptBase = OptBase::VideoRate;
}

impl OptValue for VideoRate {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::video_rate(s.trim()) {
            Some(v) => {
                self.0 = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        let _ = write!(out, "{}/{}", self.0.num, self.0.den);
    }

    fn as_f64(&self) -> Option<f64> {
        Some(self.0.to_f64())
    }

    impl_opt_value_common!(VideoRate);
}

// ----------------------------------------------------------------- duration

impl OptValueKind for Duration {
    const BASE: OptBase = OptBase::Duration;
}

impl OptValue for Duration {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::duration(s) {
            Some(v) => {
                *self = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(&parse::format_duration(*self));
    }

    fn as_f64(&self) -> Option<f64> {
        Some(self.0 as f64)
    }

    impl_opt_value_common!(Duration);
}

// -------------------------------------------------------------------- colour

impl OptValueKind for Rgba {
    const BASE: OptBase = OptBase::Color;
}

impl OptValue for Rgba {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        match parse::color(s.trim()) {
            Some(v) => {
                *self = v;
                Ok(())
            }
            None => Err(OptError::invalid(ctx.name, s)),
        }
    }

    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
        out.push_str(&parse::format_color(*self));
    }

    impl_opt_value_common!(Rgba);
}

// --------------------------------------------------------- Option<T>: unset

/// The idiomatic way to express `FFmpeg`'s magic `-1`/`INT_MIN` "not set"
/// defaults.
///
/// `None` serialises as `auto` when the inner base is `Bool` — `auto` is
/// genuinely distinct for options like `src_range` — and as the empty string
/// otherwise. One consequence: `Option<String>` cannot distinguish `None` from
/// `Some("")` on the wire. Neither can the C model, and the option grammar has
/// no way to write it.
impl<T: OptValueKind> OptValueKind for Option<T> {
    const BASE: OptBase = <T as OptValueKind>::BASE;
}

impl<T> OptValue for Option<T>
where
    T: OptValue + OptValueKind + Clone + Default + Sized,
{
    fn base(&self) -> OptBase {
        <T as OptValueKind>::BASE
    }

    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        let unset = if <T as OptValueKind>::BASE == OptBase::Bool {
            s.trim() == "auto"
        } else {
            s.is_empty()
        };
        if unset {
            *self = None;
            return Ok(());
        }
        let mut v = self.clone().unwrap_or_default();
        v.parse_into(s, ctx)?;
        *self = Some(v);
        Ok(())
    }

    fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>) {
        match self {
            Some(v) => v.serialize(out, ctx),
            None => {
                if <T as OptValueKind>::BASE == OptBase::Bool {
                    out.push_str("auto");
                }
            }
        }
    }

    fn as_f64(&self) -> Option<f64> {
        self.as_ref().and_then(OptValue::as_f64)
    }

    fn eq_dyn(&self, other: &dyn OptValue) -> bool {
        match other.as_any().downcast_ref::<Self>() {
            Some(o) => match (self, o) {
                (None, None) => true,
                (Some(a), Some(b)) => a.eq_dyn(b),
                _ => false,
            },
            None => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn clone_box(&self) -> Box<dyn OptValue> {
        Box::new(self.clone())
    }

    fn assign_from(&mut self, src: &dyn OptValue) -> bool {
        match src.as_any().downcast_ref::<Self>() {
            Some(v) => {
                self.clone_from(v);
                true
            }
            None => false,
        }
    }
}

// --------------------------------------------------------------- Vec<T>: array

/// The array modifier. `sep`, `min_len` and `max_len` come from the descriptor
/// through [`ParseCtx::array`], so one impl serves every base.
///
/// Elements are escaped one level deeper than the value itself, which is what
/// lets `channel_map=0|1|2` nest inside `scale=…:channel_map=0\|1`.
impl<T: OptValueKind> OptValueKind for Vec<T> {
    const BASE: OptBase = <T as OptValueKind>::BASE;
}

impl<T> OptValue for Vec<T>
where
    T: OptValue + OptValueKind + Clone + Default + Sized,
{
    fn base(&self) -> OptBase {
        <T as OptValueKind>::BASE
    }

    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
        let desc = ctx.array.unwrap_or_default();
        let mut out: Vec<T> = Vec::new();
        if !s.is_empty() {
            let parts = escape::split(s, desc.sep).map_err(|e| OptError::Escape {
                name: ctx.name.to_owned(),
                detail: e.to_string(),
            })?;
            for p in parts {
                let mut v = T::default();
                v.parse_into(&p, ctx)?;
                out.push(v);
            }
        }
        let len = u32::try_from(out.len()).unwrap_or(u32::MAX);
        if len < desc.min_len || len > desc.max_len {
            return Err(OptError::ArrayLen {
                name: ctx.name.to_owned(),
                len,
                min: desc.min_len,
                max: desc.max_len,
            });
        }
        *self = out;
        Ok(())
    }

    fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>) {
        let desc = ctx.array.unwrap_or_default();
        let mut special = String::new();
        special.push(desc.sep);
        for (i, v) in self.iter().enumerate() {
            if i > 0 {
                out.push(desc.sep);
            }
            let mut elem = String::new();
            v.serialize(&mut elem, ctx);
            out.push_str(&escape::escape(&elem, &special, escape::Mode::Auto));
        }
    }

    fn eq_dyn(&self, other: &dyn OptValue) -> bool {
        match other.as_any().downcast_ref::<Self>() {
            Some(o) => self.len() == o.len() && self.iter().zip(o.iter()).all(|(a, b)| a.eq_dyn(b)),
            None => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn clone_box(&self) -> Box<dyn OptValue> {
        Box::new(self.clone())
    }

    fn assign_from(&mut self, src: &dyn OptValue) -> bool {
        match src.as_any().downcast_ref::<Self>() {
            Some(v) => {
                self.clone_from(v);
                true
            }
            None => false,
        }
    }
}

// -------------------------------------------------------------- flag syntax

/// Apply `FFmpeg`'s `+flag-flag` accumulate/remove grammar to `cur`.
///
/// A leading `+` or `-` means "modify the current value"; anything else means
/// "replace it". Tokens are looked up in `ctx.consts` first and parsed as
/// integers second, so `+fast+0x10` works.
///
/// # Errors
///
/// [`OptError::UnknownConst`] when a token is neither a named constant of the
/// unit nor an integer.
pub fn parse_flag_bits(cur: u64, s: &str, ctx: &ParseCtx<'_>) -> Result<u64, OptError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    let starts_relative = s.starts_with('+') || s.starts_with('-');
    let mut acc = if starts_relative { cur } else { 0 };

    // With no leading sign the first token ORs into zero.
    let mut rest = s;
    let mut add = true;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('+') {
            add = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('-') {
            add = false;
            rest = r;
        }
        let end = rest.find(['+', '-']).unwrap_or(rest.len());
        let (tok, tail) = rest.split_at(end);
        rest = tail;
        let tok = tok.trim();
        if tok.is_empty() {
            return Err(OptError::invalid(ctx.name, s));
        }
        let bits = match lookup_const(ctx, tok) {
            Some(c) => c.value.as_i64().map(|v| v as u64),
            None => parse_integer(tok).map(|v| v as u64),
        }
        .ok_or_else(|| OptError::UnknownConst {
            name: ctx.name.to_owned(),
            value: tok.to_owned(),
        })?;
        if add {
            acc |= bits;
        } else {
            acc &= !bits;
        }
    }
    Ok(acc)
}

/// Render a bitmask as `a+b`, falling back to hex for bits no constant covers.
///
/// The output always re-parses to the same value: the first token carries no
/// sign, so parsing starts from zero and ORs each name in turn.
#[allow(
    clippy::missing_panics_doc,
    reason = "writing into a String cannot fail"
)]
pub fn serialize_flag_bits(bits: u64, out: &mut String, ctx: &SerCtx<'_>) {
    let mut covered = 0u64;
    let mut n = 0usize;
    for c in ctx.consts {
        let Some(v) = c.value.as_i64().map(|v| v as u64) else {
            continue;
        };
        if v != 0 && bits & v == v && covered & v != v {
            if n > 0 {
                out.push('+');
            }
            out.push_str(c.name);
            covered |= v;
            n += 1;
        }
    }
    let leftover = bits & !covered;
    if leftover != 0 {
        if n > 0 {
            out.push('+');
        }
        let _ = write!(out, "0x{leftover:x}");
        n += 1;
    }
    if n == 0 {
        out.push('0');
    }
}
