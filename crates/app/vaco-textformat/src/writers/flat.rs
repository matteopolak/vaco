//! The `flat` writer.
//!
//! ```text
//! streams.stream.0.index=0
//! streams.stream.0.tags.NASTY="v=1,c:2|q\"3\\4;e[f]#g <&> ünï"
//! ```
//!
//! No headers, no footers, one line per field. Two rules carry all the weight:
//!
//! * **Keys are sanitised, values are not.** Every character of a key outside
//!   `[A-Za-z0-9_]` becomes `_`, one per character. Case survives.
//! * **Integers print bare, strings print quoted.** This is the same per-field
//!   `int`/`str` decision that decides `json`'s number-versus-string, and it is
//!   why `pts=-1024` sits next to `size="258"` in the same packet.
//!
//! `hierarchical=0` drops the array sections from the path — `stream.0.index`
//! rather than `streams.stream.0.index` — but keeps everything else, including
//! the array index and any non-array child section (`format.tags.title`).

use vaco_core::Result;

use crate::escape::{escape_flat, sanitise_flat_key};
use crate::opts::{CommonOpts, parse_bool, parse_char, unknown_option};
use crate::sections::SectionFlags;
use crate::{Ctx, Out, TextWriter, WriterFlags};

/// `-of flat[=sep_char=…:hierarchical=…]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlatWriter {
    /// `sep_char`/`s`. Default `.`. Never escaped when it occurs in a value.
    pub sep_char: char,
    /// `hierarchical`/`h`. Default `1`.
    pub hierarchical: bool,
    /// The shared `sv`/`svr` pair. Unused: this writer rejects nothing.
    pub common: CommonOpts,
}

impl Default for FlatWriter {
    fn default() -> Self {
        Self {
            sep_char: '.',
            hierarchical: true,
            common: CommonOpts::default(),
        }
    }
}

impl FlatWriter {
    /// Apply `-of flat=…` options.
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
                "sep_char" | "s" => w.sep_char = parse_char(k, v)?,
                "hierarchical" | "h" => w.hierarchical = parse_bool(k, v)?,
                _ => return Err(unknown_option("flat", k)),
            }
        }
        Ok(w)
    }

    /// The `sep_char`-joined section chain, including the trailing separator.
    pub(crate) fn path(sep: char, hierarchical: bool, ctx: &Ctx<'_>) -> String {
        let mut out = String::new();
        for (i, s) in ctx.stack.iter().enumerate() {
            let parent_is_array = i
                .checked_sub(1)
                .and_then(|p| ctx.stack.get(p))
                .is_some_and(|p| p.flags.contains(SectionFlags::ARRAY));
            if !s.in_path(hierarchical) {
                continue;
            }
            out.push_str(s.name);
            out.push(sep);
            if parent_is_array {
                let idx = ctx.elem_index.get(i).copied().unwrap_or(0);
                out.push_str(&idx.to_string());
                out.push(sep);
            }
        }
        out
    }

    fn line(&self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, value: &str) -> Result<()> {
        o.s(&Self::path(self.sep_char, self.hierarchical, ctx))?;
        o.s(&sanitise_flat_key(key))?;
        o.c('=')?;
        o.s(value)?;
        o.c('\n')
    }
}

impl TextWriter for FlatWriter {
    fn name(&self) -> &'static str {
        "flat"
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::empty()
    }

    fn section_header(&mut self, _o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        Ok(())
    }

    fn section_footer(&mut self, _o: &mut Out<'_>, _ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        Ok(())
    }

    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.line(o, ctx, key, &crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        let quoted = format!("\"{}\"", escape_flat(v));
        self.line(o, ctx, key, &quoted)
    }
}
