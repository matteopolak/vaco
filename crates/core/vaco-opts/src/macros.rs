//! Declarative macros. `opt_flags!` lives here rather than in the proc-macro
//! crate because it is a straightforward token expansion with no attribute
//! grammar to parse.

/// Declare a bitmask option type whose bits are named constants of one `unit`.
///
/// `bitflags` cannot be used for this (plan 11 §3.2): our flag types must carry
/// a per-flag name, help string and unit so `-h filter=x` can print them, and
/// the option schema has to be generated from the same declaration. Adopting
/// `bitflags` would mean declaring every flag twice.
///
/// ```
/// vaco_opts::opt_flags! {
///     /// engine flags
///     #[unit = "swr_flags"]
///     pub struct SwrFlags: u64 {
///         /// force resampling even when the rates match
///         const RES = 1 << 0 => "res";
///     }
/// }
/// assert_eq!(SwrFlags::RES.bits(), 1);
/// ```
#[macro_export]
macro_rules! opt_flags {
    (
        $(#[doc = $sdoc:literal])*
        #[unit = $unit:literal]
        $vis:vis struct $name:ident : u64 {
            $(
                $(#[doc = $fdoc:literal])*
                const $cname:ident = $val:expr => $sname:literal;
            )*
        }
    ) => {
        $(#[doc = $sdoc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        $vis struct $name(u64);

        impl $name {
            $(
                $(#[doc = $fdoc])*
                pub const $cname: Self = Self($val);
            )*

            /// The unit these constants are grouped under.
            pub const UNIT: &'static str = $unit;

            #[must_use]
            pub const fn empty() -> Self { Self(0) }
            #[must_use]
            pub const fn from_bits(bits: u64) -> Self { Self(bits) }
            #[must_use]
            pub const fn bits(self) -> u64 { self.0 }
            #[must_use]
            pub const fn is_empty(self) -> bool { self.0 == 0 }
            #[must_use]
            pub const fn contains(self, o: Self) -> bool { self.0 & o.0 == o.0 }
            #[must_use]
            pub const fn intersects(self, o: Self) -> bool { self.0 & o.0 != 0 }
            #[must_use]
            pub const fn union(self, o: Self) -> Self { Self(self.0 | o.0) }
            #[must_use]
            pub const fn difference(self, o: Self) -> Self { Self(self.0 & !o.0) }
            pub fn insert(&mut self, o: Self) { self.0 |= o.0; }
            pub fn remove(&mut self, o: Self) { self.0 &= !o.0; }
        }

        impl ::core::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { self.union(rhs) }
        }

        impl $crate::OptEnumConsts for $name {
            const CONSTS: &'static [$crate::ConstDesc] = &[
                $(
                    $crate::ConstDesc {
                        name: $sname,
                        help: ::core::concat!($($fdoc),*),
                        unit: $unit,
                        value: $crate::ConstValue::Int($val as i64),
                        flags: $crate::OptFlags::NONE,
                    },
                )*
            ];
        }

        impl $crate::OptValueKind for $name {
            const BASE: $crate::OptBase = $crate::OptBase::Flags;
        }

        impl $crate::OptValue for $name {
            fn parse_into(
                &mut self,
                s: &str,
                ctx: &$crate::ParseCtx<'_>,
            ) -> ::core::result::Result<(), $crate::OptError> {
                self.0 = $crate::parse_flag_bits(self.0, s, ctx)?;
                ::core::result::Result::Ok(())
            }

            fn serialize(&self, out: &mut ::std::string::String, ctx: &$crate::SerCtx<'_>) {
                $crate::serialize_flag_bits(self.0, out, ctx);
            }

            fn as_f64(&self) -> ::core::option::Option<f64> {
                ::core::option::Option::Some(self.0 as f64)
            }

            $crate::impl_opt_value_common!($name);
        }
    };
}
