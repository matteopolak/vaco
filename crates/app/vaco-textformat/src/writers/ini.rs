//! The `ini` writer.
//!
//! ```text
//! # ffprobe output
//!
//! [streams.stream.0]
//! index=0
//!
//! [streams.stream.0.tags]
//! NASTY=v\=1,c\:2|q"3\\4;e[f]\#g <&> ünï
//! ```
//!
//! # The blank lines
//!
//! Plan 14 §4.3 says "a `\n` is emitted before every section header, including
//! wrappers". That is **not** what 8.1 does, and the difference is visible on
//! ordinary input. Two rules reproduce every observed case:
//!
//! 1. A `[path]` header is preceded by a blank line **unless the previous line
//!    written was also a header**. So `[format]` after a field block gets one,
//!    and `[streams.stream.0.tags]` immediately after `[streams.stream.0]`
//!    does not.
//! 2. A section that produced **no output at all** writes one blank line when
//!    it closes. Only empty wrappers and arrays can do this, and it is what
//!    makes `-show_entries stream=index` open with three blank lines: the
//!    empty `programs` array, the empty `stream_groups` array, and then rule 1
//!    in front of `[streams.stream.0]`.
//!
//! Compare `-of ini -show_entries stream=index` (three blanks) against
//! `-of ini -show_entries stream_tags=NASTY` (one) — the second selects only a
//! unique section name, so the two empty arrays are never opened. Getting this
//! wrong is a byte diff on the most ordinary invocation there is, so both rules
//! have dedicated tests.

use vaco_core::Result;

use crate::escape::escape_ini;
use crate::opts::{CommonOpts, parse_bool, unknown_option};
use crate::sections::SectionFlags;
use crate::writers::flat::FlatWriter;
use crate::{Ctx, Out, TextWriter, WriterFlags};

/// The fixed document prologue.
const PROLOGUE: &str = "# ffprobe output\n";

/// `-of ini[=hierarchical=…]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IniWriter {
    /// `hierarchical`/`h`. Default `1`. Behaves exactly as in `flat`.
    pub hierarchical: bool,
    /// The shared `sv`/`svr` pair. Unused: this writer rejects nothing.
    pub common: CommonOpts,
    /// Rule 1's state: was the last line written a `[path]` header?
    last_was_header: bool,
}

impl Default for IniWriter {
    fn default() -> Self {
        Self {
            hierarchical: true,
            common: CommonOpts::default(),
            last_was_header: false,
        }
    }
}

impl IniWriter {
    /// Apply `-of ini=…` options.
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
                "hierarchical" | "h" => w.hierarchical = parse_bool(k, v)?,
                _ => return Err(unknown_option("ini", k)),
            }
        }
        Ok(w)
    }

    /// Whether the section prints a `[path]` header: everything but the root
    /// and the arrays.
    fn prints_header(ctx: &Ctx<'_>) -> bool {
        !ctx.cur()
            .flags
            .intersects(SectionFlags::WRAPPER | SectionFlags::ARRAY)
    }

    fn field(&mut self, o: &mut Out<'_>, key: &str, value: &str) -> Result<()> {
        // Keys are NOT escaped here — `WE-IRD_KEY.1=x` is what the reference
        // prints, `=` and `.` and all. Only values are.
        o.s(key)?;
        o.c('=')?;
        o.s(&escape_ini(value))?;
        o.c('\n')?;
        self.last_was_header = false;
        Ok(())
    }
}

impl TextWriter for IniWriter {
    fn name(&self) -> &'static str {
        "ini"
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::DOCUMENT
    }

    fn init(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        o.s(PROLOGUE)?;
        self.last_was_header = false;
        Ok(())
    }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        if !Self::prints_header(ctx) {
            return Ok(());
        }
        if !self.last_was_header {
            o.c('\n')?;
        }
        o.c('[')?;
        let path = FlatWriter::path('.', self.hierarchical, ctx);
        // `path` ends with the separator; the header wants it without.
        o.s(path.strip_suffix('.').unwrap_or(&path))?;
        o.s("]\n")?;
        self.last_was_header = true;
        Ok(())
    }

    fn section_footer(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, produced: bool) -> Result<()> {
        if !produced {
            o.c('\n')?;
            self.last_was_header = false;
        }
        Ok(())
    }

    fn print_int(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.field(o, key, &crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        self.field(o, key, v)
    }
}
