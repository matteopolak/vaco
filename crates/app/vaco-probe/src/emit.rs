//! Driving a [`Field`] table into a [`TextFormat`].
//!
//! One place decides what an *absent* value does, because the reference's rule
//! is not the one the obvious API suggests. `vaco_textformat`'s `str_opt(k,
//! None)` prints `N/A`; the reference prints `unknown` for the colour fields
//! and `unspecified` for `chroma_location`, through the same optional path —
//! so `json` and `xml` omit them while `flat` and `default` show the word. That
//! is `print_str_opt(key, placeholder)` rather than `print_str_opt(key, "N/A")`,
//! and it is why [`Emit::put`] applies the policy itself instead of calling
//! `str_opt`.
//!
//! The policy, measured on ffprobe 8.1:
//!
//! | `-show_optional_fields` | `json`/`xml` | every other writer |
//! |---|---|---|
//! | `never` | omit | omit |
//! | `auto` (default) | omit | print the placeholder |
//! | `always` | print the placeholder | print the placeholder |
//!
//! ```sh
//! # the run that separates the three columns:
//! ffprobe -v quiet -of json -show_optional_fields always -show_streams av.mp4
//! ffprobe -v quiet -of flat                              -show_streams av.mp4
//! ```

use std::io::Write;

use vaco_core::Result;
use vaco_textformat::num::Unit;
use vaco_textformat::{OptionalFields, TextFormat, WriterFlags};

use crate::fields::{Absent, Field, Ty};

/// A value on its way to a field.
#[derive(Clone, Debug)]
pub enum Val {
    /// An integer field's value.
    I(i64),
    /// A string field's value, already spelled by `vaco_textformat::num` or by
    /// a `name()` on a model enum.
    S(String),
    /// A `Time`, `Size` or `BitRate` field's value, before formatting.
    F(f64),
    /// The value is not available.
    Absent,
}

impl Val {
    /// A string value from anything that can become one.
    pub fn s(v: impl Into<String>) -> Self {
        Self::S(v.into())
    }

    /// `Some(v)` or [`Val::Absent`].
    pub fn opt_s(v: Option<impl Into<String>>) -> Self {
        v.map_or(Self::Absent, Self::s)
    }

    /// `Some(v)` or [`Val::Absent`].
    #[must_use]
    pub fn opt_i(v: Option<i64>) -> Self {
        v.map_or(Self::Absent, Self::I)
    }

    /// `Some(v)` or [`Val::Absent`].
    #[must_use]
    pub fn opt_f(v: Option<f64>) -> Self {
        v.map_or(Self::Absent, Self::F)
    }
}

/// A [`TextFormat`] plus the optional-field policy the field table needs.
#[derive(Debug)]
pub struct Emit<'a, W: Write> {
    tf: &'a mut TextFormat<W>,
    policy: OptionalFields,
    /// Whether the writer omits unavailable optional fields (`json`, `xml`).
    suppresses: bool,
}

impl<'a, W: Write> Emit<'a, W> {
    /// Wrap a formatter. `policy` is `-show_optional_fields`.
    pub fn new(tf: &'a mut TextFormat<W>, policy: OptionalFields) -> Self {
        let suppresses = tf.flags().contains(WriterFlags::SUPPRESS_OPTIONAL);
        Self {
            tf,
            policy,
            suppresses,
        }
    }

    /// The wrapped formatter, for section open/close and for `tag`.
    pub fn tf(&mut self) -> &mut TextFormat<W> {
        self.tf
    }

    /// Emit one field of a table.
    ///
    /// A `field` of `None` — a name the table does not carry — emits nothing.
    /// That keeps a table/emitter mismatch a test failure rather than a panic.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn put(&mut self, field: Option<&'static Field>, val: &Val) -> Result<()> {
        let Some(field) = field else { return Ok(()) };
        match val {
            Val::I(v) => self.tf.int(field.name, *v),
            Val::S(v) => self.tf.str(field.name, v),
            Val::F(v) => match field.ty {
                Ty::Time => self.tf.duration(field.name, Some(*v)),
                Ty::Size => self.tf.value(field.name, Some(*v), Unit::Byte),
                Ty::BitRate => self.tf.value(field.name, Some(*v), Unit::BitPerSecond),
                // A float reaching an Int or Str field is a table bug, and the
                // honest response is to print nothing rather than to invent a
                // spelling that would then be wrong in one writer only.
                Ty::Int | Ty::Str => Ok(()),
            },
            Val::Absent => self.absent(field),
        }
    }

    /// Emit a field by name from `table`. The common call shape.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn field(&mut self, table: &'static [Field], name: &str, val: &Val) -> Result<()> {
        self.put(crate::fields::find(table, name), val)
    }

    fn absent(&mut self, field: &'static Field) -> Result<()> {
        // `Omit` is "the reference has no `print_*_opt` call for this at all";
        // `Never` is "the table swears the value is always there". Different
        // statements, same output, and emitting `N/A` for either would be a
        // guess that hides the divergence behind a plausible token.
        let placeholder = match field.absent {
            Absent::Omit | Absent::Never => return Ok(()),
            Absent::Na => vaco_textformat::num::NA,
            Absent::Word(w) => w,
        };
        match self.policy {
            OptionalFields::Never => Ok(()),
            OptionalFields::Auto if self.suppresses => Ok(()),
            OptionalFields::Auto | OptionalFields::Always => self.tf.str(field.name, placeholder),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use vaco_textformat::sections::SectionId;
    use vaco_textformat::{FormatOpts, writers};

    fn render(
        spec: &str,
        policy: OptionalFields,
        f: impl FnOnce(&mut Emit<'_, Vec<u8>>),
    ) -> String {
        let w = writers::make(spec).expect("writer");
        let opts = FormatOpts {
            show_optional_fields: policy,
            ..FormatOpts::default()
        };
        let mut tf = TextFormat::new(w, Vec::new(), opts);
        tf.open(SectionId::ROOT).expect("root");
        tf.open(SectionId::STREAMS).expect("streams");
        tf.open(SectionId::STREAM).expect("stream");
        {
            let mut e = Emit::new(&mut tf, policy);
            f(&mut e);
        }
        tf.close().expect("stream");
        tf.close().expect("streams");
        tf.close().expect("root");
        String::from_utf8(tf.finish().expect("finish")).expect("utf8")
    }

    fn colour(e: &mut Emit<'_, Vec<u8>>) {
        let t = crate::fields::STREAM;
        e.field(t, "index", &Val::I(0)).expect("index");
        e.field(t, "color_range", &Val::Absent).expect("range");
        e.field(t, "chroma_location", &Val::Absent).expect("chroma");
        e.field(t, "max_bit_rate", &Val::Absent).expect("mbr");
        e.field(t, "mime_codec_string", &Val::Absent).expect("mime");
    }

    #[test]
    fn auto_prints_the_word_for_a_flat_writer() {
        // The whole point of the module: `unknown`/`unspecified`, not `N/A`.
        assert_eq!(
            render("flat", OptionalFields::Auto, colour),
            "streams.stream.0.index=0\n\
             streams.stream.0.color_range=\"unknown\"\n\
             streams.stream.0.chroma_location=\"unspecified\"\n\
             streams.stream.0.max_bit_rate=\"N/A\"\n"
        );
    }

    #[test]
    fn auto_omits_for_json() {
        let out = render("json", OptionalFields::Auto, colour);
        assert!(out.contains("\"index\": 0"), "{out}");
        assert!(!out.contains("color_range"), "{out}");
        assert!(!out.contains("N/A"), "{out}");
    }

    #[test]
    fn always_prints_the_word_even_for_json() {
        let out = render("json", OptionalFields::Always, colour);
        assert!(out.contains("\"color_range\": \"unknown\""), "{out}");
        assert!(
            out.contains("\"chroma_location\": \"unspecified\""),
            "{out}"
        );
        assert!(out.contains("\"max_bit_rate\": \"N/A\""), "{out}");
    }

    #[test]
    fn never_omits_everywhere() {
        for spec in ["flat", "json", "default", "compact", "csv", "ini", "xml"] {
            let out = render(spec, OptionalFields::Never, colour);
            assert!(!out.contains("color_range"), "{spec}: {out}");
            assert!(!out.contains("N/A"), "{spec}: {out}");
        }
    }

    #[test]
    fn an_omit_field_is_never_printed_at_any_policy() {
        for policy in [
            OptionalFields::Auto,
            OptionalFields::Always,
            OptionalFields::Never,
        ] {
            for spec in ["flat", "json"] {
                let out = render(spec, policy, colour);
                assert!(!out.contains("mime_codec_string"), "{spec} {policy:?}");
            }
        }
    }

    #[test]
    fn an_unknown_field_name_emits_nothing() {
        let out = render("flat", OptionalFields::Always, |e| {
            e.field(crate::fields::STREAM, "nonesuch", &Val::I(1))
                .expect("nonesuch");
        });
        assert_eq!(out, "");
    }

    #[test]
    fn a_float_into_an_integer_field_emits_nothing() {
        let out = render("flat", OptionalFields::Always, |e| {
            e.field(crate::fields::STREAM, "index", &Val::F(1.0))
                .expect("index");
        });
        assert_eq!(out, "");
    }

    #[test]
    fn size_and_bitrate_go_through_their_units() {
        let out = render("flat", OptionalFields::Auto, |e| {
            e.field(crate::fields::PACKET, "size", &Val::F(258.0))
                .expect("size");
        });
        assert_eq!(out, "streams.stream.0.size=\"258\"\n");
    }
}
