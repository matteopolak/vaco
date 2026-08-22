//! The option-system error taxonomy.

use thiserror::Error;

/// Every way an option operation can fail.
///
/// The variants are the ones plan 11 §6.3 names, plus [`OptError::TypeMismatch`]
/// and [`OptError::Escape`], which the typed accessors and the escaping layer
/// need and which the plan's sketch omitted.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum OptError {
    #[error("Option not found: {name}")]
    NotFound { name: String },

    #[error("Invalid value for {name}: {value}")]
    InvalidValue { name: String, value: String },

    #[error("Value {value} for {name} out of range {min}..{max}")]
    OutOfRange {
        name: String,
        value: f64,
        min: f64,
        max: f64,
    },

    #[error("Option {name} is read-only")]
    ReadOnly { name: String },

    #[error("Option {name} is not settable at runtime")]
    NotRuntime { name: String },

    #[error("Unknown constant '{value}' for option {name}")]
    UnknownConst { name: String, value: String },

    #[error("Array for {name} has {len} elements, expected {min}..{max}")]
    ArrayLen {
        name: String,
        len: u32,
        min: u32,
        max: u32,
    },

    #[error("Too many positional arguments for {class}")]
    TooManyPositional { class: &'static str },

    /// A positional value appeared after a named one, which the filtergraph
    /// grammar forbids.
    #[error("Positional argument after a named one, in {class}")]
    PositionalAfterNamed { class: &'static str },

    /// `set_typed`/`get_typed` was called with a type that is not the option's
    /// declared field type.
    #[error("Option {name} is not of the requested type")]
    TypeMismatch { name: String },

    /// The value string is not well formed under the escaping grammar.
    #[error("Malformed escaping in {name}: {detail}")]
    Escape { name: String, detail: String },
}

impl OptError {
    pub(crate) fn invalid(name: &str, value: &str) -> Self {
        Self::InvalidValue {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    pub(crate) fn not_found(name: &str) -> Self {
        Self::NotFound {
            name: name.to_owned(),
        }
    }
}
