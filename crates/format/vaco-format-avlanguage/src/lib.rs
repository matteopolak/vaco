//! Language code normalisation: ISO 639-1, ISO 639-2 (bibliographic and
//! terminology), BCP-47, and the legacy Macintosh numeric language code.
//!
//! This is **not** a demuxer and registers no component — `vaco-demux-matroska`,
//! `vaco-demux-mp4`, `vaco-demux-mxf`, `vaco-demux-flv`/`asf`, `vaco-demux-mpegts`
//! and any subtitle format that carries a language tag call into this crate the
//! way a container calls into `vaco-format-riff` or `vaco-format-isom`.
//!
//! # Why one crate for four spellings
//!
//! The same language shows up in a real corpus spelled four different ways
//! depending on which container wrote it:
//!
//! | Spelling | Who writes it | Example |
//! |---|---|---|
//! | ISO 639-1 (two letters) | common `-metadata language=` convention | `en` |
//! | ISO 639-2/B (bibliographic, three letters) | Matroska's `Language` element default | `ger` |
//! | ISO 639-2/T (terminology, three letters) | MP4's `esds`/`elng`, modern Matroska `LanguageIETF` fallback | `deu` |
//! | BCP-47 | Matroska `LanguageIETF`, MP4 `elng`, `WebVTT` | `en-US`, `zh-Hans-CN` |
//! | Macintosh numeric code | pre-`elng` `QuickTime` `udta` text tracks | `0` (English) |
//!
//! so that `eng`, `en`, `en-US` and Macintosh code `0` all resolve to the same
//! [`ResolvedLanguage`], per plan `18-formats.md`'s brief for this crate. Without one
//! shared resolver, every container either duplicates the table or reports the
//! four spellings as four different languages.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`table`] | the ISO 639 table: 639-1, 639-2/B, 639-2/T, English name |
//! | [`mac`] | the legacy Macintosh numeric language code |
//!
//! # What this crate does not attempt
//!
//! Full BCP-47 (RFC 5646) parsing and canonicalisation — script subtags,
//! extended language subtags, variants, extensions, private-use tags in their
//! full generality — is a much larger grammar than any container in this
//! workspace actually exercises. [`parse`] extracts the primary language
//! subtag (the only part every one of the containers above actually reads
//! back) and passes the rest through unlowered as `region`. A future need for
//! the fuller grammar is an extension to [`parse`], not a redesign: nothing
//! downstream depends on the shortcut.
//!
//! # Example
//!
//! ```
//! use vaco_format_avlanguage::parse;
//!
//! let a = parse("eng").unwrap();
//! let b = parse("en").unwrap();
//! let c = parse("en-US").unwrap();
//! let d = parse("0").unwrap(); // Macintosh numeric code for English
//! assert_eq!(a.entry.iso639_2t, "eng");
//! assert_eq!(b.entry.iso639_2t, "eng");
//! assert_eq!(c.entry.iso639_2t, "eng");
//! assert_eq!(c.region.as_deref(), Some("US"));
//! assert_eq!(d.entry.iso639_2t, "eng");
//! ```

#![forbid(unsafe_code)]

pub mod mac;
pub mod table;

pub use table::LanguageEntry;

/// A normalised language, as resolved by [`parse`].
///
/// Named distinctly from `vaco_format_isom::lang::Language` (D19): that type
/// is the *packed* representation inside an MP4 `mdhd`/`udta` field and
/// deliberately does not resolve a Macintosh code to a name — its own docs
/// say the policy for that belongs elsewhere. This is that elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLanguage {
    pub entry: &'static LanguageEntry,
    /// The region/script/variant subtag(s) that followed the primary language
    /// subtag in a BCP-47 input, verbatim apart from ASCII case, or `None`
    /// when the input carried none (every non-BCP-47 spelling, and a bare
    /// two-letter/three-letter BCP-47 primary tag).
    ///
    /// Case per BCP-47 convention (RFC 5646 §2.1.1: language lowercase,
    /// script title case, region uppercase) is applied only to a two-letter
    /// region subtag, which is the one every consumer in this workspace
    /// actually reads back (`ffprobe` never prints a script subtag). Longer
    /// or multi-part remainders are passed through unchanged rather than
    /// guessed at.
    pub region: Option<String>,
}

/// Resolve any of the four spellings [`crate` docs] describe to one
/// [`ResolvedLanguage`].
///
/// Recognises, in order:
/// 1. A bare non-negative integer: a legacy Macintosh numeric language code
///    ([`mac::to_639_2t`]).
/// 2. A two-letter code: ISO 639-1.
/// 3. A three-letter code: ISO 639-2, either bibliographic or terminology
///    spelling.
/// 4. A three-letter code in ISO 639-2's private-use range (`qaa`..=`qtz`):
///    returned as-is with no table entry, since the specification assigns it
///    no fixed meaning.
/// 5. Anything containing `-`: a BCP-47 tag — the primary subtag is resolved
///    by one of the rules above, and everything after the first `-` is kept
///    verbatim as [`ResolvedLanguage::region`].
///
/// Matching is case-insensitive throughout (`ffmpeg`'s own `-metadata
/// language=` accepts either case). `None` for a code this table does not
/// cover and that is not private-use or numeric.
#[must_use]
pub fn parse(input: &str) -> Option<ResolvedLanguage> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some((primary, rest)) = input.split_once('-') {
        let entry = resolve_primary(primary)?;
        let region = if rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_alphabetic()) {
            Some(rest.to_ascii_uppercase())
        } else {
            Some(rest.to_owned())
        };
        return Some(ResolvedLanguage { entry, region });
    }
    resolve_primary(input).map(|entry| ResolvedLanguage {
        entry,
        region: None,
    })
}

/// The private-use-range synthetic entries. `qaa`..=`qtz` carries no
/// registered meaning, so this is the one place an entry is manufactured
/// rather than looked up — always self-referential (both codes equal the
/// input, lowercased) because that is the only thing a private-use code can
/// stably mean.
fn resolve_primary(code: &str) -> Option<&'static LanguageEntry> {
    if let Ok(n) = code.parse::<u16>() {
        let t = mac::to_639_2t(n)?;
        return find_by_639_2(t);
    }
    match code.len() {
        2 => find_by_639_1(code),
        3 => {
            if table::is_private_use(code) {
                // A private-use code is not a name in the table and must not
                // be confused with one, so it is handled by the caller
                // directly rather than synthesising a `'static` entry here.
                None
            } else {
                find_by_639_2(code)
            }
        }
        _ => None,
    }
}

/// Look up by ISO 639-1, case-insensitively.
#[must_use]
pub fn find_by_639_1(code: &str) -> Option<&'static LanguageEntry> {
    table::LANGUAGES
        .iter()
        .find(|e| e.iso639_1.is_some_and(|c| c.eq_ignore_ascii_case(code)))
}

/// Look up by ISO 639-2, matching either the bibliographic or the
/// terminology spelling, case-insensitively.
#[must_use]
pub fn find_by_639_2(code: &str) -> Option<&'static LanguageEntry> {
    table::LANGUAGES
        .iter()
        .find(|e| e.iso639_2b.eq_ignore_ascii_case(code) || e.iso639_2t.eq_ignore_ascii_case(code))
}

/// `input`'s ISO 639-2 terminology code — MP4's spelling — or the private-use
/// code itself (lowercased) when `input` is one, or `None` when neither
/// resolves. Accepts every spelling [`parse`] does, including a full BCP-47
/// tag (the region is discarded).
#[must_use]
pub fn to_639_2t(input: &str) -> Option<String> {
    if table::is_private_use(input.trim()) {
        return Some(input.trim().to_ascii_lowercase());
    }
    parse(input).map(|l| l.entry.iso639_2t.to_owned())
}

/// `input`'s ISO 639-2 bibliographic code — Matroska's default spelling — or
/// the private-use code itself (lowercased), or `None`. Accepts every
/// spelling [`parse`] does.
#[must_use]
pub fn to_639_2b(input: &str) -> Option<String> {
    if table::is_private_use(input.trim()) {
        return Some(input.trim().to_ascii_lowercase());
    }
    parse(input).map(|l| l.entry.iso639_2b.to_owned())
}

/// `input`'s ISO 639-1 code, when the language has one. Accepts every
/// spelling [`parse`] does.
#[must_use]
pub fn to_639_1(input: &str) -> Option<&'static str> {
    parse(input).and_then(|l| l.entry.iso639_1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_no_duplicate_639_1_codes() {
        for (i, e) in table::LANGUAGES.iter().enumerate() {
            let Some(c) = e.iso639_1 else { continue };
            let dup = table::LANGUAGES
                .iter()
                .take(i)
                .any(|o| o.iso639_1.is_some_and(|o| o.eq_ignore_ascii_case(c)));
            assert!(!dup, "duplicate iso639_1 code {c}");
        }
    }

    #[test]
    fn eng_en_and_en_us_agree() {
        let a = parse("eng").unwrap();
        let b = parse("en").unwrap();
        let c = parse("en-US").unwrap();
        assert_eq!(a.entry, b.entry);
        assert_eq!(a.entry, c.entry);
        assert_eq!(c.region.as_deref(), Some("US"));
        assert_eq!(b.region, None);
    }

    #[test]
    fn macintosh_code_zero_is_english() {
        let d = parse("0").unwrap();
        assert_eq!(d.entry.iso639_2t, "eng");
    }

    #[test]
    fn bibliographic_and_terminology_spellings_both_resolve_to_the_same_entry() {
        let ger = parse("ger").unwrap();
        let deu = parse("deu").unwrap();
        assert_eq!(ger.entry, deu.entry);
        assert_eq!(ger.entry.name, "German");
        assert_eq!(to_639_2b("deu").as_deref(), Some("ger"));
        assert_eq!(to_639_2t("ger").as_deref(), Some("deu"));
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(parse("ENG"), parse("eng"));
        assert_eq!(parse("En"), parse("en"));
    }

    #[test]
    fn private_use_codes_pass_through() {
        assert!(table::is_private_use("qaa"));
        assert!(table::is_private_use("QTZ"));
        assert!(!table::is_private_use("eng"));
        assert_eq!(to_639_2t("qaa").as_deref(), Some("qaa"));
        assert_eq!(parse("qaa"), None); // no table entry, and that is correct
    }

    #[test]
    fn special_codes_round_trip() {
        for code in ["und", "mul", "zxx", "mis"] {
            assert_eq!(to_639_2t(code).as_deref(), Some(code));
        }
    }

    #[test]
    fn unknown_codes_are_none() {
        assert_eq!(parse("xx"), None);
        assert_eq!(parse("xyz"), None);
        assert_eq!(parse(""), None);
        assert_eq!(to_639_1("xyz"), None);
    }

    #[test]
    fn a_bcp47_tag_with_a_non_region_suffix_is_kept_verbatim() {
        let l = parse("zh-Hans").unwrap();
        assert_eq!(l.entry.iso639_2t, "zho");
        assert_eq!(l.region.as_deref(), Some("Hans"));
    }
}
