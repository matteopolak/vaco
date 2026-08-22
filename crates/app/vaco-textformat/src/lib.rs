//! Output writers: `default`, `compact`, `csv`, `flat`, `ini`, `json`, `xml`.
//!
//! This crate is the v0.1 acceptance surface (D5): `ffprobe`'s output must be
//! **byte-identical** to the reference binary's (D6), and this is where every
//! byte is decided. A trailing space is a failure, so every rule here is an
//! *observation* of ffprobe 8.1, not a design.
//!
//! # The model
//!
//! Output is a tree of **sections** (`sections`), and the caller drives it with
//! a cursor:
//!
//! ```
//! use vaco_textformat::{FormatOpts, TextFormat, sections::SectionId, writers};
//!
//! let mut tf = TextFormat::new(
//!     writers::make("compact").expect("known writer"),
//!     Vec::new(),
//!     FormatOpts::default(),
//! );
//! tf.open(SectionId::ROOT)?;
//! tf.open(SectionId::STREAMS)?;
//! tf.open(SectionId::STREAM)?;
//! tf.int("index", 0)?;
//! tf.str("codec_name", "aac")?;
//! tf.close()?; // stream
//! tf.close()?; // streams
//! tf.close()?; // root
//! assert_eq!(tf.finish()?, b"stream|index=0|codec_name=aac\n");
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! Three things are load-bearing and easy to get wrong:
//!
//! * **[`TextFormat::int`] versus [`TextFormat::str`] is a property of the
//!   field, not of the value.** It is what makes `json` print `"channels": 1`
//!   next to `"sample_rate": "44100"`, and what makes `flat` print `index=0`
//!   next to `size="258"`. There is no rule to derive it from; the caller's
//!   field table decides.
//! * **Emission order is call order.** Nothing in this crate sorts, and nothing
//!   here may hold output in a map.
//! * **Only [`num`] formats a number.** Anything else drifts.
//!
//! # What each writer is
//!
//! | Writer | Shape | Options |
//! |---|---|---|
//! | `default` | `[STREAM]` … `[/STREAM]`, `key=value` | `nokey`/`nk`, `noprint_wrappers`/`nw` |
//! | `compact` | one line per section, `sep`-joined | `item_sep`/`s`, `nokey`/`nk`, `escape`/`e`, `print_section`/`p` |
//! | `csv` | `compact` with `s=,`, `nk=1`, `e=csv` | same as `compact` |
//! | `flat` | `streams.stream.0.index=0` | `sep_char`/`s`, `hierarchical`/`h` |
//! | `ini` | `[streams.stream.0]` sections | `hierarchical`/`h` |
//! | `json` | 4-space JSON | `compact`/`c` |
//! | `xml` | scalars as attributes | `fully_qualified`/`q`, `xsd_strict`/`x` |
//!
//! All seven also accept `string_validation`/`sv` and
//! `string_validation_replacement`/`svr`; only `xml` currently rejects
//! anything, because only XML has characters it cannot represent.
//!
//! # How to change it
//!
//! Change nothing here without a reference run to back it. `tests/torture.rs`
//! holds the captured bytes for one nasty string through all six writers; if a
//! change to a writer does not move that file, it did not change behaviour, and
//! if it does move it, the new bytes need a matching `ffprobe` invocation in
//! the comment above them.

#![forbid(unsafe_code)]

pub mod escape;
pub mod filter;
pub mod num;
pub mod opts;
pub mod sections;
pub mod writers;

use std::io::Write;

pub use vaco_core::Result;

use vaco_core::{Error, Rational};

pub use escape::{EscapeMode, StringValidation};
pub use filter::EntryFilterSet;
pub use num::{Pretty, Unit};
pub use opts::{FormatOpts, OptionalFields, WriterSpec};
pub use sections::{DefaultStyle, SectionDesc, SectionFlags, SectionId};

use sections::desc;

/// The byte sink a writer appends to.
///
/// Deliberately thin: writers must not buffer output across calls, because a
/// buffer is where ordering bugs hide.
pub struct Out<'a> {
    sink: &'a mut dyn Write,
}

impl std::fmt::Debug for Out<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Out").finish_non_exhaustive()
    }
}

impl Out<'_> {
    /// Append a string.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn s(&mut self, s: &str) -> Result<()> {
        self.sink.write_all(s.as_bytes()).map_err(Error::Io)
    }

    /// Append one character.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn c(&mut self, c: char) -> Result<()> {
        let mut buf = [0u8; 4];
        self.s(c.encode_utf8(&mut buf))
    }

    /// Append `n` copies of `unit`. Used for indentation.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn repeat(&mut self, unit: &str, n: usize) -> Result<()> {
        for _ in 0..n {
            self.s(unit)?;
        }
        Ok(())
    }
}

/// One entry of the section cursor.
#[derive(Clone, Debug)]
struct Frame {
    desc: &'static SectionDesc,
    /// `-show_entries` type string for a [`SectionFlags::UNIQUE_TYPE`] section.
    unique_type: Option<String>,
    /// Per-child-section counters, so array indices are per element type.
    child_counts: Vec<(SectionId, u64)>,
    /// Fields emitted directly in this section so far.
    field_index: u64,
    /// Whether this section caused any output at all. `ini` needs it.
    produced: bool,
    /// Whether the entry filter suppressed this section's fields.
    suppressed: bool,
}

/// Everything a writer needs to know about where it is.
#[derive(Debug)]
pub struct Ctx<'a> {
    /// Descriptor stack, root first. The last entry is the current section.
    pub stack: &'a [&'static SectionDesc],
    /// Array index of each stack entry within its parent.
    pub elem_index: &'a [u64],
    /// Fields already emitted in the current section.
    pub field_index: u64,
    /// The type string of the current section, when it is `UNIQUE_TYPE`.
    pub unique_type: Option<&'a str>,
    /// The run-wide formatting switches.
    pub opts: &'a FormatOpts,
}

impl Ctx<'_> {
    /// The current section. Falls back to the root for an empty stack, which
    /// cannot happen through [`TextFormat`].
    #[must_use]
    pub fn cur(&self) -> &'static SectionDesc {
        self.stack
            .last()
            .copied()
            .unwrap_or_else(|| desc(SectionId::ROOT))
    }

    /// The current section's parent, if it is not the root.
    #[must_use]
    pub fn parent(&self) -> Option<&'static SectionDesc> {
        let n = self.stack.len();
        if n < 2 {
            None
        } else {
            self.stack.get(n - 2).copied()
        }
    }

    /// Depth below the root: the root itself is 0.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len().saturating_sub(1)
    }

    /// The current section's array index within its parent.
    #[must_use]
    pub fn index(&self) -> u64 {
        self.elem_index.last().copied().unwrap_or(0)
    }

    /// The chain of enclosing sections that the inline writers flatten, from
    /// outermost to innermost, ending at the current section.
    ///
    /// One level deep in every observed case (`tags` inside `stream`), but the
    /// chain form costs nothing and cannot be wrong.
    #[must_use]
    pub fn inline_chain(&self, compact: bool) -> Vec<&'static SectionDesc> {
        let style = |s: &&'static SectionDesc| {
            if compact {
                s.compact_style()
            } else {
                s.default_style
            }
        };
        let mut chain: Vec<_> = self
            .stack
            .iter()
            .rev()
            .take_while(|s| style(s) == DefaultStyle::Inline)
            .copied()
            .collect();
        chain.reverse();
        chain
    }
}

bitflags::bitflags! {
    /// Writer capabilities the façade needs to know about.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct WriterFlags: u8 {
        /// Omit unavailable optional fields when `-show_optional_fields` is
        /// `auto`. Set for `json` and `xml`; the other writers print `N/A`.
        const SUPPRESS_OPTIONAL = 1 << 0;
        /// The writer emits a document prologue and epilogue.
        const DOCUMENT = 1 << 1;
    }
}

/// One output format.
///
/// Implementations hold only their own option values and line state. All
/// section bookkeeping lives in [`TextFormat`] and arrives through [`Ctx`].
pub trait TextWriter: std::fmt::Debug {
    /// The `-of` name.
    fn name(&self) -> &'static str;

    /// Capability flags.
    fn flags(&self) -> WriterFlags;

    /// Reject a run configuration the writer cannot represent.
    ///
    /// Only `xml=xsd_strict=1` uses this, and it is fatal: ffprobe prints the
    /// message and exits 1.
    ///
    /// # Errors
    /// [`Error::Option`] naming the offending option.
    fn validate(&self, _opts: &FormatOpts) -> Result<()> {
        Ok(())
    }

    /// Document prologue.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn init(&mut self, _o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        Ok(())
    }

    /// Document epilogue.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn fini(&mut self, _o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once per section, before any of its fields or children.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()>;

    /// Called once per section, after everything inside it.
    ///
    /// `produced` is false when the section emitted nothing whatsoever — the
    /// `ini` writer turns that into a blank line.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn section_footer(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, produced: bool) -> Result<()>;

    /// An integer-typed field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()>;

    /// A string-typed field. The value is already fully formatted.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()>;
}

/// The caller-facing façade.
///
/// Owns the section cursor, applies the `-show_entries` filter and the
/// optional-field policy, and formats every number, so that no writer ever
/// implements policy.
#[derive(Debug)]
pub struct TextFormat<W: Write> {
    writer: Box<dyn TextWriter>,
    sink: W,
    opts: FormatOpts,
    filter: EntryFilterSet,
    stack: Vec<Frame>,
    /// Scratch mirror of `stack`, so `Ctx` can borrow a plain slice.
    descs: Vec<&'static SectionDesc>,
    indices: Vec<u64>,
    finished: bool,
}

impl<W: Write> TextFormat<W> {
    /// Build a formatter with no `-show_entries` filter.
    ///
    /// # Panics
    /// Never. Use [`TextFormat::with_filter`] to supply a filter.
    #[must_use]
    pub fn new(writer: Box<dyn TextWriter>, sink: W, opts: FormatOpts) -> Self {
        Self::with_filter(writer, sink, opts, EntryFilterSet::all())
    }

    /// Build a formatter with a `-show_entries` filter.
    #[must_use]
    pub fn with_filter(
        writer: Box<dyn TextWriter>,
        sink: W,
        opts: FormatOpts,
        filter: EntryFilterSet,
    ) -> Self {
        Self {
            writer,
            sink,
            opts,
            filter,
            stack: Vec::new(),
            descs: Vec::new(),
            indices: Vec::new(),
            finished: false,
        }
    }

    /// Reject a run configuration the writer cannot represent.
    ///
    /// # Errors
    /// [`Error::Option`] from the writer.
    pub fn validate(&self) -> Result<()> {
        self.writer.validate(&self.opts)
    }

    /// The writer's capability flags.
    #[must_use]
    pub fn flags(&self) -> WriterFlags {
        self.writer.flags()
    }

    /// The writer's `-of` name.
    #[must_use]
    pub fn writer_name(&self) -> &'static str {
        self.writer.name()
    }

    /// Finish the document and return the sink.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn finish(mut self) -> Result<W> {
        if !self.finished {
            self.finished = true;
            self.with_ctx(|w, o, ctx| w.fini(o, ctx))?;
        }
        Ok(self.sink)
    }

    /// Open a section.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn open(&mut self, id: SectionId) -> Result<()> {
        self.open_inner(id, None)
    }

    /// Open a [`SectionFlags::UNIQUE_TYPE`] section, naming its type.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn open_typed(&mut self, id: SectionId, ty: &str) -> Result<()> {
        self.open_inner(id, Some(ty.to_owned()))
    }

    fn open_inner(&mut self, id: SectionId, ty: Option<String>) -> Result<()> {
        let d = desc(id);
        let elem_index = match self.stack.last_mut() {
            None => 0,
            Some(parent) => {
                let slot = parent.child_counts.iter_mut().find(|(cid, _)| *cid == id);
                if let Some((_, n)) = slot {
                    let cur = *n;
                    *n += 1;
                    cur
                } else {
                    parent.child_counts.push((id, 1));
                    0
                }
            }
        };

        let suppressed = !self.filter.section_visible(&self.descs, d);

        self.stack.push(Frame {
            desc: d,
            unique_type: ty,
            child_counts: Vec::new(),
            field_index: 0,
            produced: false,
            suppressed,
        });
        self.descs.push(d);
        self.indices.push(elem_index);

        if self.stack.len() == 1 {
            self.with_ctx(|w, o, ctx| w.init(o, ctx))?;
        }
        if !suppressed {
            self.with_ctx(|w, o, ctx| w.section_header(o, ctx))?;
            if d.default_style != DefaultStyle::Transparent {
                self.mark_produced();
            }
        }
        Ok(())
    }

    /// Close the innermost open section.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when nothing is open; otherwise the sink's error.
    pub fn close(&mut self) -> Result<()> {
        let Some(frame) = self.stack.last().cloned() else {
            return Err(Error::InvalidData("textformat: close without open"));
        };
        if !frame.suppressed {
            let produced = frame.produced;
            self.with_ctx(move |w, o, ctx| w.section_footer(o, ctx, produced))?;
        }
        self.stack.pop();
        self.descs.pop();
        self.indices.pop();
        if !frame.suppressed {
            self.mark_produced();
        }
        Ok(())
    }

    fn mark_produced(&mut self) {
        if let Some(f) = self.stack.last_mut() {
            f.produced = true;
        }
    }

    fn with_ctx<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut dyn TextWriter, &mut Out<'_>, &Ctx<'_>) -> Result<()>,
    {
        let Self {
            writer,
            sink,
            opts,
            stack,
            descs,
            indices,
            ..
        } = self;
        let top = stack.last();
        let ctx = Ctx {
            stack: descs,
            elem_index: indices,
            field_index: top.map_or(0, |f| f.field_index),
            unique_type: top.and_then(|f| f.unique_type.as_deref()),
            opts,
        };
        let mut out = Out { sink };
        f(writer.as_mut(), &mut out, &ctx)
    }

    /// Emit an integer-typed field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn int(&mut self, key: &str, v: i64) -> Result<()> {
        if !self.field_visible(key) {
            return Ok(());
        }
        let key = key.to_owned();
        self.with_ctx(move |w, o, ctx| w.print_int(o, ctx, &key, v))?;
        self.bump_field();
        Ok(())
    }

    /// Emit a string-typed field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn str(&mut self, key: &str, v: &str) -> Result<()> {
        if !self.field_visible(key) {
            return Ok(());
        }
        let (key, v) = (key.to_owned(), v.to_owned());
        self.with_ctx(move |w, o, ctx| w.print_str(o, ctx, &key, &v))?;
        self.bump_field();
        Ok(())
    }

    /// Emit a variable-key field of a `VAR_FIELDS` section. Always a string.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn tag(&mut self, key: &str, v: &str) -> Result<()> {
        self.str(key, v)
    }

    /// Emit an optional integer field, honouring `-show_optional_fields`.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn int_opt(&mut self, key: &str, v: Option<i64>) -> Result<()> {
        match v {
            Some(v) => self.int(key, v),
            None => self.unavailable(key),
        }
    }

    /// Emit an optional string field, honouring `-show_optional_fields`.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn str_opt(&mut self, key: &str, v: Option<&str>) -> Result<()> {
        match v {
            Some(v) => self.str(key, v),
            None => self.unavailable(key),
        }
    }

    /// Emit a pre-formatted field.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn fmt(&mut self, key: &str, args: std::fmt::Arguments<'_>) -> Result<()> {
        match args.as_str() {
            Some(s) => self.str(key, s),
            None => self.str(key, &args.to_string()),
        }
    }

    fn unavailable(&mut self, key: &str) -> Result<()> {
        match self.opts.show_optional_fields {
            OptionalFields::Never => Ok(()),
            OptionalFields::Always => self.str(key, num::NA),
            OptionalFields::Auto => {
                if self.writer.flags().contains(WriterFlags::SUPPRESS_OPTIONAL) {
                    Ok(())
                } else {
                    self.str(key, num::NA)
                }
            }
        }
    }

    fn bump_field(&mut self) {
        if let Some(f) = self.stack.last_mut() {
            f.field_index += 1;
            f.produced = true;
        }
    }

    fn field_visible(&self, key: &str) -> bool {
        match self.stack.last() {
            None => false,
            Some(f) => !f.suppressed && self.filter.field_visible(&self.descs, f.desc, key),
        }
    }

    // ---------------------------------------------------------- domain helpers

    /// A raw integer timestamp (`pts`, `dts`, `start_pts`).
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn ts(&mut self, key: &str, ts: Option<i64>) -> Result<()> {
        self.int_opt(key, ts)
    }

    /// A `*_time` field: a timestamp rescaled by its time base and printed as
    /// seconds, honouring `-sexagesimal` / `-unit` / `-prefix`.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn time(&mut self, key: &str, ts: Option<i64>, tb: Rational) -> Result<()> {
        let secs = ts.and_then(|t| {
            if tb.den == 0 {
                None
            } else {
                Some(t as f64 * f64::from(tb.num) / f64::from(tb.den))
            }
        });
        self.duration(key, secs)
    }

    /// A duration already in seconds.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn duration(&mut self, key: &str, secs: Option<f64>) -> Result<()> {
        match secs {
            None => self.unavailable(key),
            Some(s) => {
                let text = num::time(s, self.opts.pretty);
                self.str(key, &text)
            }
        }
    }

    /// A scalar with a unit — a size or a bit rate.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn value(&mut self, key: &str, v: Option<f64>, unit: Unit) -> Result<()> {
        match v {
            None => self.unavailable(key),
            Some(v) => {
                let text = num::value(v, unit, self.opts.pretty);
                self.str(key, &text)
            }
        }
    }

    /// A `num/den` rational.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn rational(&mut self, key: &str, r: Rational) -> Result<()> {
        let text = num::rational(r);
        self.str(key, &text)
    }

    /// A `num:den` aspect ratio.
    ///
    /// # Errors
    /// Propagates the sink's I/O error.
    pub fn ratio(&mut self, key: &str, r: Rational) -> Result<()> {
        let text = num::ratio(r);
        self.str(key, &text)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn render(spec: &str, f: impl FnOnce(&mut TextFormat<Vec<u8>>) -> Result<()>) -> String {
        let w = writers::make(spec).expect("writer spec");
        let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
        f(&mut tf).expect("emit");
        String::from_utf8(tf.finish().expect("finish")).expect("utf8")
    }

    fn one_stream(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
        tf.open(SectionId::ROOT)?;
        tf.open(SectionId::STREAMS)?;
        tf.open(SectionId::STREAM)?;
        tf.int("index", 0)?;
        tf.str("codec_name", "aac")?;
        tf.open(SectionId::STREAM_TAGS)?;
        tf.tag("language", "und")?;
        tf.close()?;
        tf.close()?;
        tf.close()?;
        tf.close()
    }

    #[test]
    fn default_shape() {
        assert_eq!(
            render("default", one_stream),
            "[STREAM]\nindex=0\ncodec_name=aac\nTAG:language=und\n[/STREAM]\n"
        );
    }

    #[test]
    fn compact_shape() {
        assert_eq!(
            render("compact", one_stream),
            "stream|index=0|codec_name=aac|tag:language=und\n"
        );
    }

    #[test]
    fn csv_shape() {
        assert_eq!(render("csv", one_stream), "stream,0,aac,und\n");
    }

    #[test]
    fn flat_shape() {
        assert_eq!(
            render("flat", one_stream),
            "streams.stream.0.index=0\nstreams.stream.0.codec_name=\"aac\"\n\
             streams.stream.0.tags.language=\"und\"\n"
        );
    }

    #[test]
    fn ini_shape() {
        assert_eq!(
            render("ini", one_stream),
            "# ffprobe output\n\n[streams.stream.0]\nindex=0\ncodec_name=aac\n\n\
             [streams.stream.0.tags]\nlanguage=und\n"
        );
    }

    #[test]
    fn json_shape() {
        assert_eq!(
            render("json", one_stream),
            "{\n    \"streams\": [\n        {\n            \"index\": 0,\n            \
             \"codec_name\": \"aac\",\n            \"tags\": {\n                \
             \"language\": \"und\"\n            }\n        }\n    ]\n}\n"
        );
    }

    #[test]
    fn xml_shape() {
        assert_eq!(
            render("xml", one_stream),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ffprobe>\n    <streams>\n        \
             <stream index=\"0\" codec_name=\"aac\">\n            <tags>\n                \
             <tag key=\"language\" value=\"und\"/>\n            </tags>\n        </stream>\n    \
             </streams>\n</ffprobe>\n"
        );
    }

    #[test]
    fn close_without_open_is_an_error() {
        let w = writers::make("default").expect("writer");
        let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
        assert!(tf.close().is_err());
    }
}
