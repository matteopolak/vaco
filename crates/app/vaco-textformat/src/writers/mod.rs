//! The six writers, plus the `-of` name → writer mapping.
//!
//! `csv` is not a seventh writer: it is [`compact::CompactWriter`] with four
//! different defaults (`s=,`, `nk=1`, `e=csv`, and its own `-of` name).

mod compact;
mod default;
mod flat;
mod ini;
mod json;
mod xml;

pub use compact::CompactWriter;
pub use default::DefaultWriter;
pub use flat::FlatWriter;
pub use ini::IniWriter;
pub use json::JsonWriter;
pub use xml::XmlWriter;

use vaco_core::{Error, Result};

use crate::TextWriter;
use crate::opts::WriterSpec;

/// The `-of` names this crate implements, in ffprobe's listing order.
pub const NAMES: [&str; 7] = ["default", "compact", "csv", "flat", "ini", "json", "xml"];

/// Build a writer from an `-of` argument such as `compact=s=,:nk=1`.
///
/// # Errors
/// [`Error::Option`] for an unknown writer name, an unknown writer option, or
/// an option value that does not parse.
pub fn make(spec: &str) -> Result<Box<dyn TextWriter>> {
    let spec = WriterSpec::parse(spec)?;
    let opts = &spec.options;
    match spec.name.as_str() {
        "default" => Ok(Box::new(DefaultWriter::from_options(opts)?)),
        "compact" => Ok(Box::new(CompactWriter::compact_defaults(opts)?)),
        "csv" => Ok(Box::new(CompactWriter::csv_defaults(opts)?)),
        "flat" => Ok(Box::new(FlatWriter::from_options(opts)?)),
        "ini" => Ok(Box::new(IniWriter::from_options(opts)?)),
        "json" => Ok(Box::new(JsonWriter::from_options(opts)?)),
        "xml" => Ok(Box::new(XmlWriter::from_options(opts)?)),
        other => Err(Error::Option {
            name: "output_format".to_owned(),
            detail: format!("unknown writer {other:?}"),
        }),
    }
}

/// Lowercase a section-type string into the shape `compact` uses as a key
/// qualifier: every character outside `[a-z0-9_]` becomes `_`, **one
/// underscore per character**.
///
/// `H.26[45] User Data Unregistered SEI message` becomes
/// `h_26_45__user_data_unregistered_sei_message` — note the double underscore
/// from `] `, which is what proves the substitution is per character and not
/// per run.
#[must_use]
pub fn sanitise_type(ty: &str) -> String {
    ty.chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn observed_type_sanitisation() {
        assert_eq!(sanitise_type("Skip Samples"), "skip_samples");
        assert_eq!(
            sanitise_type("H.26[45] User Data Unregistered SEI message"),
            "h_26_45__user_data_unregistered_sei_message"
        );
    }

    #[test]
    fn every_name_builds() {
        for n in NAMES {
            let w = make(n).expect("writer");
            assert_eq!(w.name(), n);
        }
    }

    #[test]
    fn unknown_writer_and_option_are_errors() {
        assert!(make("yaml").is_err());
        assert!(make("json=nosuch=1").is_err());
        assert!(make("ini=h=maybe").is_err());
    }
}
