//! The `xml` writer.
//!
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <ffprobe>
//!     <streams>
//!         <stream index="0">
//!             <tags>
//!                 <tag key="language" value="und"/>
//!             </tags>
//!         </stream>
//!     </streams>
//! </ffprobe>
//! ```
//!
//! **Scalars are attributes, nested sections are child elements** — the XSD
//! convention, and it holds throughout.
//!
//! # The `<stream >` quirk
//!
//! A section that *can* carry attributes is opened as `<name ` — with the
//! trailing space already written — and the attributes are appended one after
//! another separated by a single space. So `<stream index="0"/>` looks normal,
//! and a stream with **no** attributes comes out as `<stream >` (with children)
//! or `<stream />` (without). Both are observed, both are reproduced, both are
//! pinned by a test.
//!
//! Sections that can only hold child elements — every array, and every
//! variable-field section — skip the space entirely: `<streams>`, `<tags>`,
//! and, for a section with a unique type, `<side_data type="Skip Samples">`.
//! Those never self-close, even when empty.
//!
//! # String validation
//!
//! XML 1.0 cannot represent the C0 controls other than tab, LF and CR, so this
//! is the one writer for which `string_validation`/`sv` does anything. The
//! default is `replace`, and the default replacement is **U+FFFD** — not the
//! empty string the documentation claims. `sv=fail` drops the whole field.

use vaco_core::{Error, Result};

use crate::escape::{escape_xml, validate_xml};
use crate::opts::{CommonOpts, parse_bool, unknown_option};
use crate::sections::SectionFlags;
use crate::{Ctx, FormatOpts, Out, TextWriter, WriterFlags};

const INDENT: &str = "    ";
const PROLOGUE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
const QUALIFIED_ROOT: &str = concat!(
    "<ffprobe:ffprobe xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
    "xmlns:ffprobe=\"http://www.ffmpeg.org/schema/ffprobe\" ",
    "xsi:schemaLocation=\"http://www.ffmpeg.org/schema/ffprobe ffprobe.xsd\">\n"
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Level {
    /// The element name, borrowed from the schema.
    name: &'static str,
    /// The opening tag is written but not yet terminated with `>` or `/>`.
    tag_open: bool,
    /// Attributes written so far, for the separating space.
    attrs: u64,
    /// Children written so far, for the root's blank-line separator.
    children: u64,
}

/// `-of xml[=fully_qualified=…:xsd_strict=…]`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct XmlWriter {
    /// `fully_qualified`/`q`. Default `0`.
    pub fully_qualified: bool,
    /// `xsd_strict`/`x`. Default `0`; implies `fully_qualified`.
    pub xsd_strict: bool,
    /// The shared `sv`/`svr` pair. This writer is the only one that uses it.
    pub common: CommonOpts,
    levels: Vec<Level>,
}

impl XmlWriter {
    /// Apply `-of xml=…` options.
    ///
    /// # Errors
    /// [`vaco_core::Error::Option`] for an unknown key or an unparsable value.
    pub fn from_options(options: &[(String, String)]) -> Result<Self> {
        let mut w = Self::default();
        for (k, v) in options {
            if w.common.set(k, v)? {
                continue;
            }
            match k.as_str() {
                "fully_qualified" | "q" => w.fully_qualified = parse_bool(k, v)?,
                "xsd_strict" | "x" => {
                    w.xsd_strict = parse_bool(k, v)?;
                    if w.xsd_strict {
                        w.fully_qualified = true;
                    }
                }
                _ => return Err(unknown_option("xml", k)),
            }
        }
        Ok(w)
    }

    fn root_name(&self) -> &'static str {
        if self.fully_qualified {
            "ffprobe:ffprobe"
        } else {
            "ffprobe"
        }
    }

    /// Terminate the enclosing element's opening tag, so a child can follow.
    fn open_parent_tag(&mut self, o: &mut Out<'_>) -> Result<()> {
        let Some(parent) = self.levels.last_mut() else {
            return Ok(());
        };
        if parent.tag_open {
            parent.tag_open = false;
            o.s(">\n")?;
        }
        Ok(())
    }

    /// A `<name key="…" value="…"/>` child, used for variable-field sections.
    fn pair_element(&mut self, o: &mut Out<'_>, elem: &str, key: &str, v: &str) -> Result<()> {
        self.open_parent_tag(o)?;
        if let Some(parent) = self.levels.last_mut() {
            parent.children += 1;
        }
        o.repeat(INDENT, self.levels.len())?;
        o.c('<')?;
        o.s(elem)?;
        o.s(" key=\"")?;
        o.s(&escape_xml(key))?;
        o.s("\" value=\"")?;
        o.s(&escape_xml(v))?;
        o.s("\"/>\n")
    }

    fn attribute(&mut self, o: &mut Out<'_>, key: &str, v: &str) -> Result<()> {
        if self.levels.last().is_some_and(|l| l.attrs > 0) {
            o.c(' ')?;
        }
        if let Some(level) = self.levels.last_mut() {
            level.attrs += 1;
        }
        o.s(&escape_xml(key))?;
        o.s("=\"")?;
        o.s(&escape_xml(v))?;
        o.c('"')
    }

    /// Run the value through `string_validation`; [`None`] means "drop the
    /// field entirely", which is what `sv=fail` does.
    fn valid(&self, v: &str) -> Option<String> {
        validate_xml(v, self.common.validation, &self.common.replacement)
    }
}

impl TextWriter for XmlWriter {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::SUPPRESS_OPTIONAL | WriterFlags::DOCUMENT
    }

    fn validate(&self, opts: &FormatOpts) -> Result<()> {
        if !self.xsd_strict {
            return Ok(());
        }
        // Only `unit` and `prefix` are checked. `-byte_binary_prefix` is a
        // no-op in 8.1 and `-sexagesimal` is simply not tested for; both are
        // accepted under `xsd_strict=1`. Observed.
        for (set, name) in [(opts.pretty.unit, "unit"), (opts.pretty.prefix, "prefix")] {
            if set {
                return Err(Error::Option {
                    name: name.to_owned(),
                    detail: format!(
                        "XSD-compliant output selected but option '{name}' was selected, \
                         XML output may be non-compliant.\n\
                         You need to disable such option with '-no{name}'"
                    ),
                });
            }
        }
        Ok(())
    }

    fn init(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        o.s(PROLOGUE)
    }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        let cur = ctx.cur();

        if self.levels.is_empty() {
            o.s(if self.fully_qualified {
                QUALIFIED_ROOT
            } else {
                "<ffprobe>\n"
            })?;
            self.levels.push(Level {
                name: self.root_name(),
                tag_open: false,
                attrs: 0,
                children: 0,
            });
            return Ok(());
        }

        self.open_parent_tag(o)?;
        // A blank line separates the root's children.
        if self.levels.len() == 1 && self.levels.first().is_some_and(|root| root.children > 0) {
            o.c('\n')?;
        }
        if let Some(parent) = self.levels.last_mut() {
            parent.children += 1;
        }

        // Arrays and variable-field sections hold only child elements, so their
        // opening tag closes immediately after the (optional) type attribute.
        let element_only = cur
            .flags
            .intersects(SectionFlags::ARRAY | SectionFlags::VAR_FIELDS);

        o.repeat(INDENT, self.levels.len())?;
        o.c('<')?;
        o.s(cur.name)?;

        if element_only {
            if let Some(ty) = ctx.unique_type
                && cur.flags.contains(SectionFlags::UNIQUE_TYPE)
            {
                o.s(" type=\"")?;
                o.s(&escape_xml(&self.valid(ty).unwrap_or_default()))?;
                o.c('"')?;
            }
            o.s(">\n")?;
            self.levels.push(Level {
                name: cur.name,
                tag_open: false,
                attrs: 0,
                children: 0,
            });
        } else {
            o.c(' ')?;
            self.levels.push(Level {
                name: cur.name,
                tag_open: true,
                attrs: 0,
                children: 0,
            });
        }
        Ok(())
    }

    fn section_footer(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        let Some(level) = self.levels.pop() else {
            return Ok(());
        };
        if level.tag_open {
            // No children ever arrived: `<stream />`, `<packet pts="0"/>`.
            return o.s("/>\n");
        }
        o.repeat(INDENT, self.levels.len())?;
        o.s("</")?;
        o.s(level.name)?;
        o.s(">\n")
    }

    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.print_str(o, ctx, key, &crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        let (Some(key), Some(v)) = (self.valid(key), self.valid(v)) else {
            return Ok(());
        };
        if ctx.cur().flags.contains(SectionFlags::VAR_FIELDS) {
            let elem = ctx.cur().element_name.unwrap_or(ctx.cur().name);
            return self.pair_element(o, elem, &key, &v);
        }
        self.attribute(o, &key, &v)
    }
}
