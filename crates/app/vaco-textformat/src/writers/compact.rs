//! The `compact` writer, and `csv` — the same writer with different defaults.
//!
//! ```text
//! stream|index=0|codec_name=aac|tag:language=und
//! ```
//!
//! # The separator state machine
//!
//! Getting this exactly right matters more than it looks. A section header
//! writes its name **followed by** the separator, and each subsequent item
//! writes the separator **before** itself. That is why an empty section prints
//! `stream|` with a trailing separator, and why a nested `[HEADER]`-style child
//! reads `…|component|index=1` — a separator on each side of the child's name.
//!
//! A section footer always writes a newline, even when the current line is
//! already empty, which is where the blank line between `pixel_format` groups
//! comes from. All observed.

use vaco_core::Result;

use crate::escape::EscapeMode;
use crate::opts::{CommonOpts, parse_bool, parse_char, unknown_option};
use crate::sections::{DefaultStyle, SectionFlags};
use crate::writers::sanitise_type;
use crate::{Ctx, Out, TextWriter, WriterFlags};

/// `-of compact[=…]` and `-of csv[=…]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompactWriter {
    /// `item_sep`/`s`. `|` for `compact`, `,` for `csv`.
    pub item_sep: char,
    /// `nokey`/`nk`. `0` for `compact`, `1` for `csv`.
    pub nokey: bool,
    /// `escape`/`e`. `c` for `compact`, `csv` for `csv`.
    pub escape: EscapeMode,
    /// `print_section`/`p`. `1` for both.
    pub print_section: bool,
    /// The shared `sv`/`svr` pair.
    pub common: CommonOpts,
    /// `csv` differs from `compact` only in defaults and in this name.
    name: &'static str,
    /// Whether an item separator is owed before the next item.
    sep_pending: bool,
}

impl CompactWriter {
    /// `-of compact` defaults.
    ///
    /// # Errors
    /// [`vaco_core::Error::Option`] for an unknown key or an unparsable value.
    pub fn compact_defaults(options: &[(String, String)]) -> Result<Self> {
        Self::build("compact", '|', false, EscapeMode::C, options)
    }

    /// `-of csv` defaults: `s=,`, `nk=1`, `e=csv`.
    ///
    /// # Errors
    /// [`vaco_core::Error::Option`] for an unknown key or an unparsable value.
    pub fn csv_defaults(options: &[(String, String)]) -> Result<Self> {
        Self::build("csv", ',', true, EscapeMode::Csv, options)
    }

    fn build(
        name: &'static str,
        item_sep: char,
        nokey: bool,
        escape: EscapeMode,
        options: &[(String, String)],
    ) -> Result<Self> {
        let mut w = Self {
            item_sep,
            nokey,
            escape,
            print_section: true,
            common: CommonOpts::default(),
            name,
            sep_pending: false,
        };
        for (k, v) in options {
            if w.common.set(k, v)? {
                continue;
            }
            match k.as_str() {
                "item_sep" | "s" => w.item_sep = parse_char(k, v)?,
                "nokey" | "nk" => w.nokey = parse_bool(k, v)?,
                "print_section" | "p" => w.print_section = parse_bool(k, v)?,
                "escape" | "e" => {
                    w.escape = EscapeMode::parse(v).ok_or_else(|| unknown_option(name, v))?;
                }
                _ => return Err(unknown_option(name, k)),
            }
        }
        Ok(w)
    }

    /// `tag:`, `disposition:`, `side_datum/skip_samples:` — the qualifier an
    /// inlined child section puts in front of each of its keys.
    fn key_prefix(ctx: &Ctx<'_>) -> String {
        let mut out = String::new();
        for s in ctx.inline_chain(true) {
            out.push_str(s.inline_prefix());
            if let Some(ty) = ctx.unique_type
                && s.flags.contains(SectionFlags::UNIQUE_TYPE)
            {
                out.push('/');
                out.push_str(&sanitise_type(ty));
            }
            out.push(':');
        }
        out
    }

    fn item(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        if self.sep_pending {
            o.c(self.item_sep)?;
        }
        if !self.nokey {
            o.s(&Self::key_prefix(ctx))?;
            o.s(key)?;
            o.c('=')?;
        }
        o.s(&self.escape.apply(v, self.item_sep))?;
        self.sep_pending = true;
        Ok(())
    }
}

impl TextWriter for CompactWriter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::empty()
    }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        if ctx.cur().compact_style() != DefaultStyle::Header || !self.print_section {
            return Ok(());
        }
        if self.sep_pending {
            o.c(self.item_sep)?;
        }
        o.s(ctx.cur().name)?;
        o.c(self.item_sep)?;
        self.sep_pending = false;
        Ok(())
    }

    fn section_footer(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        if ctx.cur().compact_style() != DefaultStyle::Header {
            return Ok(());
        }
        o.c('\n')?;
        self.sep_pending = false;
        Ok(())
    }

    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.item(o, ctx, key, &crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        self.item(o, ctx, key, v)
    }
}
