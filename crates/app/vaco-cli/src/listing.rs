//! The `-version`, `-formats`, `-muxers`… options: print a table and exit 0.
//!
//! # Scope
//!
//! CL-04 owns the help system — `-h`, `-h long`, `-h full`, `-h <kind>=<name>`
//! and byte-identical listing output. This module is deliberately the thin
//! part: the listings that are a direct render of `vaco-registry`, so that a
//! binary which cannot say what it contains does not ship. Everything with real
//! layout work in it returns [`AvError::ENOSYS`] naming the issue rather than a
//! half-formatted table that would then have to be un-shipped.
//!
//! Output is **not** byte-identical with the reference and is not trying to be.
//! Format and codec *names* are interface facts and are reproduced (D9); the
//! column legends are prose and are ours.

use std::io::Write;

use vaco_registry::{Kind, components_of_kind};

use crate::exit::{AvError, Diagnostic};

/// This program's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The stderr banner.
///
/// The reference prints its own identity and nine library versions here.
/// Reproducing that would be claiming to be `FFmpeg`, which D9 puts outside
/// what we copy. The *shape* is the same so `-hide_banner` means the same
/// thing.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn banner<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(
        w,
        "vaco version {VERSION} Copyright (c) 2026 the Vaco authors"
    )
}

/// Render one `-<name>` listing.
///
/// # Errors
///
/// [`AvError::ENOSYS`] for a listing this build does not render yet.
pub fn render<W: Write>(w: &mut W, name: &str) -> Result<(), Diagnostic> {
    let mut go = || -> std::io::Result<bool> {
        match name {
            "version" => {
                writeln!(w, "vaco version {VERSION}")?;
            }
            "L" | "license" => {
                writeln!(w, "vaco is licensed under MIT OR Apache-2.0.")?;
            }
            "buildconf" => {
                writeln!(w, "  configuration:")?;
                for f in enabled_features() {
                    writeln!(w, "    --enable-{f}")?;
                }
            }
            "formats" => {
                writeln!(w, "File formats:")?;
                writeln!(w, " D. = demuxing supported")?;
                writeln!(w, " .E = muxing supported")?;
                writeln!(w, " --")?;
                for c in components_of_kind(Kind::Demuxer) {
                    writeln!(w, " D  {:<16} {}", c.name, c.long_name.unwrap_or(c.name))?;
                }
                for c in components_of_kind(Kind::Muxer) {
                    writeln!(w, "  E {:<16} {}", c.name, c.long_name.unwrap_or(c.name))?;
                }
            }
            "demuxers" | "muxers" | "decoders" | "encoders" | "filters" | "bsfs" | "protocols"
            | "parsers" => {
                let kind = kind_of(name);
                writeln!(w, "{}:", heading(name))?;
                for c in components_of_kind(kind) {
                    writeln!(w, " {:<16} {}", c.name, c.long_name.unwrap_or(c.name))?;
                }
            }
            "codecs" => {
                writeln!(w, "Codecs:")?;
                for id in vaco_registry::codecs() {
                    let d = if vaco_registry::can_decode(id) {
                        'D'
                    } else {
                        '.'
                    };
                    writeln!(w, " {d}. {}", id.name())?;
                }
            }
            "dispositions" => {
                for &(_, n) in vaco_cli_core::Disposition::ALL {
                    writeln!(w, "{n}")?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    };
    match go() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Diagnostic::new(
            AvError::ENOSYS,
            vec![format!(
                "-{name} is not implemented in this build; the help and listing surface is issue CL-04."
            )],
        )),
        Err(e) => Err(Diagnostic::new(
            AvError::of(&vaco_core::Error::Io(e)),
            vec!["Error writing to standard output".to_owned()],
        )),
    }
}

fn kind_of(name: &str) -> Kind {
    match name {
        "muxers" => Kind::Muxer,
        "decoders" => Kind::Decoder,
        "encoders" => Kind::Encoder,
        "filters" => Kind::Filter,
        "bsfs" => Kind::BitstreamFilter,
        "protocols" => Kind::Protocol,
        "parsers" => Kind::Parser,
        _ => Kind::Demuxer,
    }
}

fn heading(name: &str) -> &'static str {
    match name {
        "muxers" => "Muxers",
        "decoders" => "Decoders",
        "encoders" => "Encoders",
        "filters" => "Filters",
        "bsfs" => "Bitstream filters",
        "protocols" => "Protocols",
        "parsers" => "Parsers",
        _ => "Demuxers",
    }
}

fn enabled_features() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vaco_registry::components()
        .filter_map(|c| c.feature)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn text(name: &str) -> String {
        let mut buf = Vec::new();
        render(&mut buf, name).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn version_prints_our_identity_not_the_references() {
        let s = text("version");
        assert!(s.starts_with("vaco version "), "{s}");
        assert!(!s.contains("FFmpeg"));
    }

    #[test]
    fn formats_lists_the_demuxers_this_build_has_and_no_muxers() {
        let s = text("formats");
        assert!(s.contains("matroska"), "{s}");
        // D5: zero muxers. The listing has to say so rather than imply one.
        assert_eq!(vaco_registry::muxers().len(), 0);
        assert!(!s.lines().any(|l| l.starts_with("  E ")), "{s}");
    }

    #[test]
    fn dispositions_are_the_nineteen_names_in_bit_order() {
        let s = text("dispositions");
        assert_eq!(s.lines().count(), 19);
        assert_eq!(s.lines().next(), Some("default"));
        assert_eq!(s.lines().last(), Some("multilayer"));
    }

    #[test]
    fn an_unimplemented_listing_names_the_issue_rather_than_half_rendering() {
        let mut buf = Vec::new();
        let e = render(&mut buf, "pix_fmts").unwrap_err();
        assert!(e.render().contains("CL-04"), "{}", e.render());
        assert!(buf.is_empty());
    }

    #[test]
    fn the_banner_is_ours() {
        let mut buf = Vec::new();
        banner(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("vaco version "));
        assert!(!s.contains("ffmpeg"));
    }
}
