//! The ISO 639 language table.
//!
//! Source: ISO 639-2 Registration Authority (Library of Congress,
//! <https://www.loc.gov/standards/iso639-2/php/code_list.php>), which is the
//! canonical public registry for the two three-letter codes, and ISO 639-1
//! for the two-letter code. This is registration data — a code, its name,
//! and (for twenty languages) two different three-letter spellings the
//! registration authority itself assigns — not anyone's creative expression,
//! and it is reproduced from that registry rather than from any tool's
//! internal table (D9).
//!
//! # Bibliographic vs terminology codes
//!
//! Twenty languages have two different ISO 639-2 codes: a "bibliographic"
//! code (`ger`), inherited from older cataloguing conventions and the one
//! Matroska's spec names as its default, and a "terminology" code (`deu`),
//! derived from the language's native or scholarly name and the one MP4's
//! `esds`/`ISO 639-2/T` field is defined in terms of. Every other language
//! has only one three-letter code, which this table stores in both fields.
//!
//! # What is not here
//!
//! This is a working subset — the languages actually likely to appear in
//! real media metadata — not the full ISO 639-2 registry of roughly 480
//! entries (which includes collective codes for language families and
//! historical languages far outside this project's scope). Extending it is
//! adding a row; nothing else has to change.

/// One language's identity across the three code spaces this crate
/// normalises between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageEntry {
    /// ISO 639-1 two-letter code, when the language has one. Roughly the
    /// 180-odd most widely spoken/written languages have one; most of the
    /// ISO 639-2 registry does not.
    pub iso639_1: Option<&'static str>,
    /// ISO 639-2 bibliographic code (`"ger"`). Equal to `iso639_2t` for every
    /// language except the twenty B/T pairs.
    pub iso639_2b: &'static str,
    /// ISO 639-2 terminology code (`"deu"`). Equal to `iso639_2b` for every
    /// language except the twenty B/T pairs.
    pub iso639_2t: &'static str,
    /// English name, for display only — never parsed back.
    pub name: &'static str,
}

macro_rules! lang {
    ($one:expr, $code:expr, $name:expr) => {
        LanguageEntry {
            iso639_1: Some($one),
            iso639_2b: $code,
            iso639_2t: $code,
            name: $name,
        }
    };
    ($one:expr, $b:expr, $t:expr, $name:expr) => {
        LanguageEntry {
            iso639_1: Some($one),
            iso639_2b: $b,
            iso639_2t: $t,
            name: $name,
        }
    };
}

/// Three-letter-only entry (no ISO 639-1 code).
const fn lang3(code: &'static str, name: &'static str) -> LanguageEntry {
    LanguageEntry {
        iso639_1: None,
        iso639_2b: code,
        iso639_2t: code,
        name,
    }
}

/// The table. Order is not significant; lookups are linear scans over a
/// table small enough (under 150 rows) that this is faster than building and
/// maintaining a perfect-hash generator for it, and it keeps the table
/// trivially auditable against the registry.
pub const LANGUAGES: &[LanguageEntry] = &[
    // The twenty ISO 639-2 bibliographic/terminology pairs, verbatim from the
    // registration authority's own list of them.
    lang!("sq", "alb", "sqi", "Albanian"),
    lang!("hy", "arm", "hye", "Armenian"),
    lang!("eu", "baq", "eus", "Basque"),
    lang!("my", "bur", "mya", "Burmese"),
    lang!("zh", "chi", "zho", "Chinese"),
    lang!("cs", "cze", "ces", "Czech"),
    lang!("nl", "dut", "nld", "Dutch"),
    lang!("fr", "fre", "fra", "French"),
    lang!("ka", "geo", "kat", "Georgian"),
    lang!("de", "ger", "deu", "German"),
    lang!("el", "gre", "ell", "Greek"),
    lang!("is", "ice", "isl", "Icelandic"),
    lang!("mk", "mac", "mkd", "Macedonian"),
    lang!("mi", "mao", "mri", "Maori"),
    lang!("ms", "may", "msa", "Malay"),
    lang!("fa", "per", "fas", "Persian"),
    lang!("ro", "rum", "ron", "Romanian"),
    lang!("sk", "slo", "slk", "Slovak"),
    lang!("bo", "tib", "bod", "Tibetan"),
    lang!("cy", "wel", "cym", "Welsh"),
    // The rest: one code each.
    lang!("aa", "aar", "Afar"),
    lang!("af", "afr", "Afrikaans"),
    lang!("am", "amh", "Amharic"),
    lang!("ar", "ara", "Arabic"),
    lang!("as", "asm", "Assamese"),
    lang!("az", "aze", "Azerbaijani"),
    lang!("be", "bel", "Belarusian"),
    lang!("bg", "bul", "Bulgarian"),
    lang!("bn", "ben", "Bengali"),
    lang!("bs", "bos", "Bosnian"),
    lang!("ca", "cat", "Catalan"),
    lang!("co", "cos", "Corsican"),
    lang!("da", "dan", "Danish"),
    lang!("dz", "dzo", "Dzongkha"),
    lang!("en", "eng", "English"),
    lang!("eo", "epo", "Esperanto"),
    lang!("et", "est", "Estonian"),
    lang!("fi", "fin", "Finnish"),
    lang!("fo", "fao", "Faroese"),
    lang!("ga", "gle", "Irish"),
    lang!("gd", "gla", "Scottish Gaelic"),
    lang!("gl", "glg", "Galician"),
    lang!("gu", "guj", "Gujarati"),
    lang!("ha", "hau", "Hausa"),
    lang!("he", "heb", "Hebrew"),
    lang!("hi", "hin", "Hindi"),
    lang!("hr", "hrv", "Croatian"),
    lang!("ht", "hat", "Haitian"),
    lang!("hu", "hun", "Hungarian"),
    lang!("id", "ind", "Indonesian"),
    lang!("it", "ita", "Italian"),
    lang!("ja", "jpn", "Japanese"),
    lang!("jv", "jav", "Javanese"),
    lang!("kk", "kaz", "Kazakh"),
    lang!("km", "khm", "Khmer"),
    lang!("kn", "kan", "Kannada"),
    lang!("ko", "kor", "Korean"),
    lang!("ku", "kur", "Kurdish"),
    lang!("ky", "kir", "Kyrgyz"),
    lang!("la", "lat", "Latin"),
    lang!("lb", "ltz", "Luxembourgish"),
    lang!("lo", "lao", "Lao"),
    lang!("lt", "lit", "Lithuanian"),
    lang!("lv", "lav", "Latvian"),
    lang!("mg", "mlg", "Malagasy"),
    lang!("ml", "mal", "Malayalam"),
    lang!("mn", "mon", "Mongolian"),
    lang!("mr", "mar", "Marathi"),
    lang!("mt", "mlt", "Maltese"),
    lang!("ne", "nep", "Nepali"),
    lang!("no", "nor", "Norwegian"),
    lang!("ny", "nya", "Chichewa"),
    lang!("or", "ori", "Oriya"),
    lang!("pa", "pan", "Punjabi"),
    lang!("pl", "pol", "Polish"),
    lang!("ps", "pus", "Pashto"),
    lang!("pt", "por", "Portuguese"),
    lang!("qu", "que", "Quechua"),
    lang!("rm", "roh", "Romansh"),
    lang!("ru", "rus", "Russian"),
    lang!("rw", "kin", "Kinyarwanda"),
    lang!("sd", "snd", "Sindhi"),
    lang!("si", "sin", "Sinhala"),
    lang!("sl", "slv", "Slovenian"),
    lang!("sm", "smo", "Samoan"),
    lang!("sn", "sna", "Shona"),
    lang!("so", "som", "Somali"),
    lang!("sr", "srp", "Serbian"),
    lang!("su", "sun", "Sundanese"),
    lang!("sv", "swe", "Swedish"),
    lang!("sw", "swa", "Swahili"),
    lang!("ta", "tam", "Tamil"),
    lang!("te", "tel", "Telugu"),
    lang!("tg", "tgk", "Tajik"),
    lang!("th", "tha", "Thai"),
    lang!("tk", "tuk", "Turkmen"),
    lang!("tl", "tgl", "Tagalog"),
    lang!("tr", "tur", "Turkish"),
    lang!("tt", "tat", "Tatar"),
    lang!("ug", "uig", "Uyghur"),
    lang!("uk", "ukr", "Ukrainian"),
    lang!("ur", "urd", "Urdu"),
    lang!("uz", "uzb", "Uzbek"),
    lang!("vi", "vie", "Vietnamese"),
    lang!("xh", "xho", "Xhosa"),
    lang!("yi", "yid", "Yiddish"),
    lang!("yo", "yor", "Yoruba"),
    lang!("zu", "zul", "Zulu"),
    lang!("es", "spa", "Spanish"),
    // ISO 639-2 special/reserved codes: no ISO 639-1 form, but valid and
    // stable three-letter codes in their own right.
    lang3("und", "Undetermined"),
    lang3("mul", "Multiple languages"),
    lang3("zxx", "No linguistic content"),
    lang3("mis", "Uncoded languages"),
];

/// Whether `code` is in ISO 639-2's private-use range `qaa`..=`qtz`
/// (twenty codes reserved for local, project-specific use, matched
/// case-insensitively and never resolved to a name).
#[must_use]
pub fn is_private_use(code: &str) -> bool {
    let mut b = [0u8; 3];
    if code.len() != 3 {
        return false;
    }
    for (slot, c) in b.iter_mut().zip(code.bytes()) {
        *slot = c.to_ascii_lowercase();
    }
    b[0] == b'q' && (b'a'..=b't').contains(&b[1]) && b[2].is_ascii_lowercase()
}
