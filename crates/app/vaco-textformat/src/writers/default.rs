//! The `default` writer.
//!
//! ```text
//! [STREAM]
//! index=0
//! TAG:language=und
//! [/STREAM]
//! ```
//!
//! **It escapes nothing.** A tag value of `v=1,c:2|q"3\4;e[f]#g <&> ünï` comes
//! back byte for byte, control characters included. Observed.

use vaco_core::Result;

use crate::opts::{CommonOpts, parse_bool, unknown_option};
use crate::sections::DefaultStyle;
use crate::{Ctx, Out, TextWriter, WriterFlags};

/// `-of default[=nokey=…:noprint_wrappers=…]`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DefaultWriter {
    /// `nokey`/`nk`: print bare values. Drops the `TAG:` prefix too.
    pub nokey: bool,
    /// `noprint_wrappers`/`nw`: suppress `[SECTION]` and `[/SECTION]`, but not
    /// the inline prefixes.
    pub noprint_wrappers: bool,
    /// The shared `sv`/`svr` pair. Unused: this writer rejects nothing.
    pub common: CommonOpts,
}

impl DefaultWriter {
    /// Apply `-of default=…` options.
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
                "nokey" | "nk" => w.nokey = parse_bool(k, v)?,
                "noprint_wrappers" | "nw" => w.noprint_wrappers = parse_bool(k, v)?,
                _ => return Err(unknown_option("default", k)),
            }
        }
        Ok(w)
    }

    /// `TAG:`, `DISPOSITION:`, `FLAGS:` — the uppercased inline prefix chain,
    /// including its trailing colon. Empty for a `[HEADER]`-style section.
    fn prefix(ctx: &Ctx<'_>) -> String {
        let chain = ctx.inline_chain(false);
        let mut out = String::new();
        for s in chain {
            out.push_str(&s.inline_prefix().to_ascii_uppercase());
            out.push(':');
        }
        out
    }

    fn field(&self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        if self.nokey {
            o.s(v)?;
        } else {
            o.s(&Self::prefix(ctx))?;
            o.s(key)?;
            o.c('=')?;
            o.s(v)?;
        }
        o.c('\n')
    }
}

impl TextWriter for DefaultWriter {
    fn name(&self) -> &'static str {
        "default"
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::empty()
    }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        if self.noprint_wrappers || ctx.cur().default_style != DefaultStyle::Header {
            return Ok(());
        }
        o.c('[')?;
        o.s(&ctx.cur().name.to_ascii_uppercase())?;
        o.s("]\n")
    }

    fn section_footer(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        if self.noprint_wrappers || ctx.cur().default_style != DefaultStyle::Header {
            return Ok(());
        }
        o.s("[/")?;
        o.s(&ctx.cur().name.to_ascii_uppercase())?;
        o.s("]\n")
    }

    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.field(o, ctx, key, &crate::num::int(v))
    }

    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        self.field(o, ctx, key, v)
    }
}
