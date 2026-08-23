//! The legacy Macintosh numeric language code.
//!
//! Apple's *`QuickTime` File Format Specification* (published; developer
//! archive), "Language Code Values". `mov`/`mp4`'s old-style `elng`-less
//! text tracks and the classic `MacUserData`/`udta` `©`-atom convention
//! store a stream's language as a *number* into this table rather than as
//! ISO 639 text — the box-format detail plan 18 §3.4.1 flags as needing this
//! crate: "the high bit is set is a Macintosh language code, not packed
//! ISO-639, and needs the legacy table."
//!
//! Reproduced from the specification's own table, which is a fixed
//! enumeration (scenes-a-faire, D9) rather than anyone's expression. This is
//! the common subset — the languages a real file is likely to carry — not
//! every one of the spec's ~150 entries; extending it is adding a row to
//! [`MAC_LANGUAGE_TO_639_2T`].
//!
//! Several codes are one Macintosh number to one ISO 639-2 code where the
//! Mac table distinguishes a script or variant ISO 639-2 does not (19
//! "Chinese (Traditional)" and 33 "Chinese (Simplified)" both resolve to
//! `zho`; 34 "Flemish" resolves to `nld`) — that collapse is inherent in the
//! target code space, not a gap in this table.

/// `(Macintosh code, ISO 639-2/T)`.
pub const MAC_LANGUAGE_TO_639_2T: &[(u16, &str)] = &[
    (0, "eng"),
    (1, "fra"),
    (2, "deu"),
    (3, "ita"),
    (4, "nld"),
    (5, "swe"),
    (6, "spa"),
    (7, "dan"),
    (8, "por"),
    (9, "nor"),
    (10, "heb"),
    (11, "jpn"),
    (12, "ara"),
    (13, "fin"),
    (14, "ell"),
    (15, "isl"),
    (16, "mlt"),
    (17, "tur"),
    (18, "hrv"),
    (19, "zho"), // Chinese (Traditional)
    (20, "urd"),
    (21, "hin"),
    (22, "tha"),
    (23, "kor"),
    (24, "lit"),
    (25, "pol"),
    (26, "hun"),
    (27, "est"),
    (28, "lav"),
    (29, "sme"),
    (30, "fao"),
    (31, "fas"),
    (32, "rus"),
    (33, "zho"), // Chinese (Simplified)
    (34, "nld"), // Flemish
    (35, "gle"),
    (36, "sqi"),
    (37, "ron"),
    (38, "ces"),
    (39, "slk"),
    (40, "slv"),
    (41, "yid"),
    (42, "srp"),
    (43, "mkd"),
    (44, "bul"),
    (45, "ukr"),
    (46, "bel"),
    (47, "uzb"),
    (48, "kaz"),
    (51, "hye"),
    (52, "kat"),
    (54, "kir"),
    (55, "tgk"),
    (56, "tuk"),
    (57, "mon"),
    (59, "pus"),
    (60, "kur"),
    (62, "snd"),
    (63, "bod"),
    (64, "nep"),
    (66, "mar"),
    (67, "ben"),
    (68, "asm"),
    (69, "guj"),
    (70, "pan"),
    (71, "ori"),
    (72, "mal"),
    (73, "kan"),
    (74, "tam"),
    (75, "tel"),
    (76, "sin"),
    (77, "mya"),
    (78, "khm"),
    (79, "lao"),
    (80, "vie"),
    (81, "ind"),
    (82, "tgl"),
    (83, "msa"),
    (85, "amh"),
    (88, "som"),
    (89, "swa"),
    (93, "mlg"),
    (94, "epo"),
    (128, "cym"),
    (129, "eus"),
    (130, "cat"),
    (131, "lat"),
    (136, "uig"),
    (137, "dzo"),
];

/// `code`'s ISO 639-2/T equivalent, for a code this table covers.
#[must_use]
pub fn to_639_2t(code: u16) -> Option<&'static str> {
    MAC_LANGUAGE_TO_639_2T
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, t)| t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_codes_resolve() {
        assert_eq!(to_639_2t(0), Some("eng"));
        assert_eq!(to_639_2t(11), Some("jpn"));
        assert_eq!(to_639_2t(32), Some("rus"));
    }

    #[test]
    fn an_unassigned_code_is_none() {
        assert_eq!(to_639_2t(9999), None);
    }

    #[test]
    fn the_table_has_no_duplicate_keys() {
        for (i, &(c, _)) in MAC_LANGUAGE_TO_639_2T.iter().enumerate() {
            assert!(
                MAC_LANGUAGE_TO_639_2T
                    .iter()
                    .take(i)
                    .all(|&(c2, _)| c2 != c),
                "duplicate Mac language code {c}"
            );
        }
    }
}
