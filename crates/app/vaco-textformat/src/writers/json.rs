//! The `json` writer.
//!
//! ```json
//! {
//!     "streams": [
//!         {
//!             "index": 0,
//!             "sample_rate": "44100"
//!         }
//!     ]
//! }
//! ```
//!
//! Three things are not negotiable:
//!
//! * **Number versus string is a property of the field.** `"channels": 1` and
//!   `"sample_rate": "44100"` come off the same audio stream. Nothing about
//!   the value distinguishes them; the caller's `int`/`str` choice does.
//! * **An empty object or array still spans three lines**, with a blank one in
//!   the middle: an opening brace, an empty line, a closing brace. That falls
//!   out of the container opening
//!   with a brace and a newline and closing with a newline, the indent and the
//!   matching brace, and it is observable whenever
//!   `-show_entries` filters a section down to nothing.
//! * **`/` is not escaped and non-ASCII stays raw UTF-8.** `ünï` is `ünï`.
//!
//! `compact=1` puts each *object* on one line as `{ "k": v, "k2": v2 }` — note
//! the spaces just inside the braces. Arrays stay multi-line, a nested section
//! still forces a line break, and the root object is never compacted.

use vaco_core::Result;

use crate::escape::escape_json;
use crate::opts::{CommonOpts, parse_bool, unknown_option};
use crate::sections::SectionFlags;
use crate::{Ctx, Out, TextWriter, WriterFlags};

const INDENT: &str = "    ";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Level {
    is_array: bool,
    compact: bool,
    count: u64,
}

/// `-of json[=compact=…]`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct JsonWriter {
    /// `compact`/`c`. Default `0`.
    pub compact: bool,
    /// The shared `sv`/`svr` pair. Unused: this writer rejects nothing.
    pub common: CommonOpts,
    levels: Vec<Level>,
}

impl JsonWriter {
    /// Apply `-of json=…` options.
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
                "compact" | "c" => w.compact = parse_bool(k, v)?,
                _ => return Err(unknown_option("json", k)),
            }
        }
        Ok(w)
    }

    /// Emit the separator and indentation in front of the next item of the
    /// innermost container.
    ///
    /// Items of the container at depth `n` are indented `n` units, and the
    /// container's own closing brace `n - 1` — which is exactly
    /// `self.levels.len()` before and after the matching `pop`.
    fn begin_item(&mut self, o: &mut Out<'_>, is_section: bool) -> Result<()> {
        let Some(level) = self.levels.last_mut() else {
            return Ok(());
        };
        let first = level.count == 0;
        level.count += 1;
        let compact = level.compact;
        if !first {
            o.c(',')?;
        }
        if compact {
            if !is_section {
                // `{ "index": 0, "codec_name": "aac"` — one space either side
                // of the comma, and one just inside the brace.
                return o.c(' ');
            }
            // A nested section still gets its own line, except as the very
            // first item, where the space just inside the brace stands in for
            // the newline: `{             "tags": { … } }`. Observed.
            o.c(if first { ' ' } else { '\n' })?;
        } else if !first {
            o.c('\n')?;
        }
        o.repeat(INDENT, self.levels.len())
    }
}

impl TextWriter for JsonWriter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::SUPPRESS_OPTIONAL | WriterFlags::DOCUMENT
    }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        let cur = ctx.cur();
        let is_array = cur.flags.contains(SectionFlags::ARRAY);
        let parent_is_array = ctx
            .parent()
            .is_some_and(|p| p.flags.contains(SectionFlags::ARRAY));

        if !self.levels.is_empty() {
            self.begin_item(o, true)?;
            if !parent_is_array {
                o.c('"')?;
                o.s(&escape_json(cur.name))?;
                o.s("\": ")?;
            }
        }

        // Arrays are never compacted, and neither is the root object.
        let compact = self.compact && !is_array && !self.levels.is_empty();
        o.c(if is_array { '[' } else { '{' })?;
        if !compact {
            o.c('\n')?;
        }
        self.levels.push(Level {
            is_array,
            compact,
            count: 0,
        });
        Ok(())
    }

    fn section_footer(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        let Some(level) = self.levels.pop() else {
            return Ok(());
        };
        if level.compact {
            return o.s(" }");
        }
        o.c('\n')?;
        o.repeat(INDENT, self.levels.len())?;
        o.c(if level.is_array { ']' } else { '}' })
    }

    fn fini(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        o.c('\n')
    }

    fn print_int(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.begin_item(o, false)?;
        o.c('"')?;
        o.s(&escape_json(key))?;
        o.s("\": ")?;
        o.s(&crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        self.begin_item(o, false)?;
        o.c('"')?;
        o.s(&escape_json(key))?;
        o.s("\": \"")?;
        o.s(&escape_json(v))?;
        o.c('"')
    }
}
