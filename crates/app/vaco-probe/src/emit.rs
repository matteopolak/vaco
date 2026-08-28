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
    /// `-bitexact`, which drops every `*_long_name` field.
    bitexact: bool,
}

impl<'a, W: Write> Emit<'a, W> {
    /// Wrap a formatter. `policy` is `-show_optional_fields`.
    pub fn new(tf: &'a mut TextFormat<W>, policy: OptionalFields) -> Self {
        let suppresses = tf.flags().contains(WriterFlags::SUPPRESS_OPTIONAL);
        Self {
            tf,
            policy,
            suppresses,
            bitexact: false,
        }
    }

    /// Turn on `-bitexact`.
    #[must_use]
    pub const fn bitexact(mut self, on: bool) -> Self {
        self.bitexact = on;
        self
    }

    /// Whether `-bitexact` is on.
    ///
    /// Separate from [`Emit::dropped_by_bitexact`]: that answers "is this
    /// *field* suppressed", for `*_long_name`. This answers "is bitexact mode
    /// active at all", for a field that stays present but changes *value* —
    /// `profile` is the one measured case: `ffprobe -bitexact` prints the raw
    /// numeric profile (`100`) where a plain run prints the library's name
    /// (`High`), on every codec whose profile has one. Measured on H.264,
    /// AAC, VP9 and AV1; see `stream_value`'s `"profile"` arm.
    #[must_use]
    pub const fn is_bitexact(&self) -> bool {
        self.bitexact
    }

    /// Whether `-bitexact` drops this field.
    ///
    /// **It drops every `*_long_name`.** Measured on ffprobe 8.1, and nowhere
    /// documented:
    ///
    /// ```sh
    /// ffprobe -hide_banner -show_format av.mp4 | grep -c long_name           # 1
    /// ffprobe -bitexact -hide_banner -show_format av.mp4 | grep -c long_name # 0
    /// ```
    ///
    /// The reasoning is sound once you see it — a long name is descriptive
    /// prose that changes between builds, exactly what `-bitexact` exists to
    /// remove — but it is not the sort of thing anyone would guess, and the
    /// consequence is severe: every one of the differential harness's
    /// `exact-bytes` cases runs under `-bitexact`, so this single field made
    /// **156 of 198** probe cases diverge on their first run. Found by running
    /// the harness, which is the argument for having built it.
    ///
    /// Matched on the suffix rather than a list of names, deliberately:
    /// `format_long_name` and `codec_long_name` exist today and a new section
    /// with its own long name should not have to remember to come back here.
    ///
    /// Not `const`: the first version hand-rolled the suffix comparison to be
    /// `const`, which meant indexing, which the workspace denies for exactly
    /// the reason it should — a hand-rolled bounds calculation is where an
    /// off-by-one panic lives. `str::ends_with` is not const and this is called
    /// once per field, which is nothing.
    fn dropped_by_bitexact(name: &str) -> bool {
        name.ends_with("_long_name")
    }

    /// The wrapped formatter, for section open/close **only**.
    ///
    /// Field emission goes through [`Emit::put`], [`Emit::int`], [`Emit::str`]
    /// or [`Emit::tag`], because `-show_optional_fields never` is a property of
    /// fields and a call that bypasses this type bypasses it. See
    /// [`Emit::suppress_all`].
    pub fn tf(&mut self) -> &mut TextFormat<W> {
        self.tf
    }

    /// Whether `-show_optional_fields never` is in force.
    ///
    /// **It suppresses every field, not merely the unavailable ones.** Measured
    /// on ffprobe 8.1, and it is not what the option's name suggests:
    ///
    /// ```sh
    /// ffprobe -v error -of default -show_format -show_optional_fields never av.mp4
    /// #   [FORMAT]
    /// #   [/FORMAT]
    /// ffprobe -v error -of xml -show_packets -show_optional_fields never … av.mp4
    /// #   <packet />
    /// ```
    ///
    /// `filename`, `index`, `codec_type` and `flags` all go, and so do the
    /// `TAG:` and `DISPOSITION:` lines — but the **sections** stay: `json`
    /// still emits `"tags": {}` and `xml` still emits
    /// `<side_data type="Skip Samples">`. So the rule is "no fields", not "no
    /// content", and the type attribute of a typed section is not a field.
    #[must_use]
    pub const fn suppress_all(&self) -> bool {
        matches!(self.policy, OptionalFields::Never)
    }

    /// An always-present integer field, for a section with no [`Field`] table.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn int(&mut self, key: &str, v: i64) -> Result<()> {
        if self.bitexact && Self::dropped_by_bitexact(key) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.int(key, v)
    }

    /// An optional integer field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn int_opt(&mut self, key: &str, v: Option<i64>) -> Result<()> {
        if self.bitexact && Self::dropped_by_bitexact(key) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.int_opt(key, v)
    }

    /// An always-present string field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn str(&mut self, key: &str, v: &str) -> Result<()> {
        if self.bitexact && Self::dropped_by_bitexact(key) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.str(key, v)
    }

    /// A timestamp field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn ts(&mut self, key: &str, ts: Option<i64>) -> Result<()> {
        if self.bitexact && Self::dropped_by_bitexact(key) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.ts(key, ts)
    }

    /// A seconds field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn duration(&mut self, key: &str, secs: Option<f64>) -> Result<()> {
        if self.bitexact && Self::dropped_by_bitexact(key) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.duration(key, secs)
    }

    /// One key/value pair of a `tags` section.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn tag(&mut self, key: &str, v: &str) -> Result<()> {
        if self.suppress_all() {
            return Ok(());
        }
        self.tf.tag(key, v)
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
        if self.bitexact && Self::dropped_by_bitexact(field.name) {
            return Ok(());
        }
        if self.suppress_all() {
            return Ok(());
        }
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
