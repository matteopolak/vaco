//! Static option metadata: what `-h filter=scale` prints, and what the runtime
//! consults to parse a value. Every type here is `Copy` and lives in a `static`
//! so a schema can be introspected without constructing the object that owns it.

use crate::{OptBase, OptFlags, OptKind};

/// An option's index within its owning [`Schema`], in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OptId(pub u16);

/// A named constant belonging to an option's `unit`.
#[derive(Debug, Clone, Copy)]
pub struct ConstDesc {
    pub name: &'static str,
    pub help: &'static str,
    pub unit: &'static str,
    pub value: ConstValue,
    pub flags: OptFlags,
}

impl ConstDesc {
    #[must_use]
    pub const fn new(
        name: &'static str,
        help: &'static str,
        unit: &'static str,
        value: i64,
    ) -> Self {
        Self {
            name,
            help,
            unit,
            value: ConstValue::Int(value),
            flags: OptFlags::NONE,
        }
    }
}

/// Constants are `int64` or `double` in the C model; we keep both and the
/// schema says which.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
}

impl ConstValue {
    #[must_use]
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(v),
            // Only exact integral floats convert; a fractional constant is not
            // a legal value for an integer option.
            Self::Float(v) if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 => {
                Some(v as i64)
            }
            Self::Float(_) => None,
        }
    }

    #[must_use]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(v) => v as f64,
            Self::Float(v) => v,
        }
    }
}

/// The display-only numeric range. The authoritative check is typed; see
/// [`crate::Options::check_range`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptRangeDisplay {
    pub min: f64,
    pub max: f64,
}

/// One option's complete static description.
#[derive(Debug, Clone, Copy)]
pub struct OptionDesc {
    pub name: &'static str,
    /// Additional accepted spellings — the inventory's `isr`/`in_sample_rate`
    /// pattern.
    pub aliases: &'static [&'static str],
    pub help: &'static str,
    pub kind: OptKind,
    pub flags: OptFlags,
    /// The `unit` grouping named constants under this option.
    pub unit: Option<&'static str>,
    /// Named constants in this option's unit. Empty unless `unit` is set.
    pub consts: &'static [ConstDesc],
    /// For display and `query_ranges` only.
    pub range: Option<OptRangeDisplay>,
    /// Rendered default, as `-h full` prints it. Best effort at macro-expansion
    /// time; [`crate::OptionsExt::default_repr`] computes the exact form.
    pub default_repr: &'static str,
    pub id: OptId,
}

impl OptionDesc {
    /// Whether `name` is this option's primary name or one of its aliases.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }

    #[must_use]
    pub const fn base(&self) -> OptBase {
        self.kind.base
    }
}

/// A class's option table plus the child classes reachable from it.
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    pub class_name: &'static str,
    /// One-line description of the class, for `-h`. Not in plan 11 §6.3's
    /// sketch, which gave `#[options(help = "…")]` nowhere to land.
    pub class_help: &'static str,
    pub options: &'static [OptionDesc],
    /// Child schemas reachable for option lookup.
    pub children: &'static [&'static Schema],
}

impl Schema {
    /// Look an option up in this schema only, by name or alias.
    #[must_use]
    pub fn find(&'static self, name: &str) -> Option<&'static OptionDesc> {
        self.options.iter().find(|o| o.matches(name))
    }

    #[must_use]
    pub fn find_by_id(&'static self, id: OptId) -> Option<&'static OptionDesc> {
        self.options.iter().find(|o| o.id == id)
    }

    /// Look an option up here, then depth-first in the child schemas.
    #[must_use]
    pub fn find_recursive(
        &'static self,
        name: &str,
    ) -> Option<(&'static Schema, &'static OptionDesc)> {
        if let Some(d) = self.find(name) {
            return Some((self, d));
        }
        self.children.iter().find_map(|c| c.find_recursive(name))
    }

    /// Every named constant carrying `unit`, in declaration order.
    ///
    /// Constants are a property of the *unit*, not of the option: several
    /// options may share one unit, and `-h` groups the constants under each
    /// option that references it.
    pub fn consts_for_unit(&'static self, unit: &str) -> impl Iterator<Item = &'static ConstDesc> {
        let unit = unit.to_owned();
        self.options
            .iter()
            .filter(move |o| o.unit == Some(unit.as_str()))
            .flat_map(|o| o.consts.iter())
    }

    /// Iterate in declaration order. Positional filter arguments depend on this
    /// being stable.
    pub fn iter(&'static self) -> impl Iterator<Item = &'static OptionDesc> {
        self.options.iter()
    }

    /// Every option in this schema and, depth first, in its children.
    #[must_use]
    pub fn iter_recursive(
        &'static self,
    ) -> Box<dyn Iterator<Item = (&'static Schema, &'static OptionDesc)>> {
        Box::new(
            self.options
                .iter()
                .map(move |o| (self, o))
                .chain(self.children.iter().flat_map(|c| c.iter_recursive())),
        )
    }
}

impl IntoIterator for &'static Schema {
    type Item = &'static OptionDesc;
    type IntoIter = core::slice::Iter<'static, OptionDesc>;
    fn into_iter(self) -> Self::IntoIter {
        self.options.iter()
    }
}

/// Implemented by `#[derive(Options)]` so a schema can be reached from the
/// *type*, with no value in hand. This is what makes `-h filter=scale` work
/// without instantiating a filter.
pub trait HasSchema {
    const SCHEMA: &'static Schema;
}

/// Introspect a type's option schema without constructing it.
#[must_use]
pub fn schema_of<T: HasSchema>() -> &'static Schema {
    T::SCHEMA
}

/// Implemented by `#[derive(OptEnum)]` and by `opt_flags!`, contributing the
/// named constants of a unit.
pub trait OptEnumConsts {
    const CONSTS: &'static [ConstDesc];
}
