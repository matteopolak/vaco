//! Font discovery: a [`cosmic_text::FontSystem`] (which owns the `fontdb`
//! database — "fontdb via cosmic-text", plan 16 SS6.1) plus our own alias
//! table.
//!
//! fontconfig resolves `font=Sans` through system configuration and
//! per-language preference lists; `fontdb`'s own matcher does not have that,
//! so `font=Sans`/`sans-serif`/`serif`/`monospace`/`cursive`/`fantasy` are
//! mapped here to an explicit, ordered list of real family names before
//! `fontdb` ever sees them. This is a **deliberate divergence** (plan 16
//! SS6.2): the resolved face may differ from an fontconfig system's choice.
//! `fontfile=`/an exact family name both bypass this table entirely and are
//! exact on every platform.

use cosmic_text::FontSystem;
use cosmic_text::fontdb::Family;

/// Ordered fallback family names for each generic CSS-style keyword, most
/// platforms first. `fontdb`'s `Database::query` walks a family list and
/// returns the first that resolves, so listing several is free insurance
/// against any one being absent.
fn generic_fallbacks(keyword: &str) -> &'static [&'static str] {
    match keyword {
        "serif" => &[
            "Times New Roman",
            "Liberation Serif",
            "DejaVu Serif",
            "Noto Serif",
        ],
        "monospace" => &[
            "Consolas",
            "Liberation Mono",
            "DejaVu Sans Mono",
            "Menlo",
            "Noto Sans Mono",
        ],
        "cursive" => &["Comic Sans MS", "Apple Chancery", "URW Chancery L"],
        "fantasy" => &["Impact", "Papyrus"],
        // "sans-serif" / "sans" / anything unrecognised: the reference's own
        // most common fallback family.
        _ => &[
            "Arial",
            "Liberation Sans",
            "DejaVu Sans",
            "Helvetica",
            "Noto Sans",
        ],
    }
}

/// Resolve a `font=`/`fontname=` option value to a name `fontdb` should try,
/// applying the alias table for the five CSS generic keywords (case-
/// insensitive, matching the reference's own `font=Sans`/`font=sans-serif`
/// acceptance).
#[must_use]
pub fn resolve_family(requested: &str) -> Vec<String> {
    let lower = requested.trim().to_ascii_lowercase();
    match lower.as_str() {
        "sans-serif" | "sans" | "" => generic_fallbacks("sans-serif")
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        "serif" | "monospace" | "cursive" | "fantasy" => generic_fallbacks(lower.as_str())
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        _ => vec![requested.to_owned()],
    }
}

/// [`Family`] values for [`resolve_family`]'s output, borrowing from it —
/// `fontdb::Database::query` takes a family list by reference.
#[must_use]
pub fn family_list(names: &[String]) -> Vec<Family<'_>> {
    names.iter().map(|n| Family::Name(n)).collect()
}

/// Add a directory of font files to the search path (`-font_dirs`'s
/// equivalent; the reference has no such option for `drawtext`/`ass`, but
/// nothing here can reach `~/.config/fontconfig`, so a way to point at a
/// specific directory is the only way an out-of-tree font is ever found).
pub fn add_search_dir(font_system: &mut FontSystem, dir: &std::path::Path) {
    font_system.db_mut().load_fonts_dir(dir);
}

/// Load an embedded font (a Matroska `AttachedFile` payload, or any other
/// in-memory font) into the database, so subsequent shaping can select it by
/// family name like any system font.
///
/// Bounded by the caller: `bytes` is attacker-controlled (an attachment
/// inside an untrusted container), so this takes already-budget-checked data
/// rather than reading a file itself.
pub fn load_embedded(font_system: &mut FontSystem, bytes: Vec<u8>) {
    font_system.db_mut().load_font_data(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_keywords_resolve_to_more_than_one_candidate() {
        assert!(resolve_family("sans-serif").len() > 1);
        assert!(resolve_family("Sans").len() > 1);
        assert!(resolve_family("MONOSPACE").len() > 1);
    }

    #[test]
    fn an_exact_family_name_passes_through_unchanged() {
        assert_eq!(
            resolve_family("Comic Sans MS"),
            vec!["Comic Sans MS".to_owned()]
        );
    }

    #[test]
    fn empty_family_falls_back_to_sans_serif() {
        assert_eq!(resolve_family(""), resolve_family("sans-serif"));
    }
}
