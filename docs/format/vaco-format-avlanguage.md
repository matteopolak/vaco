# `vaco-format-avlanguage`

Layer 4. Language code normalisation: ISO 639-1, ISO 639-2 (bibliographic
and terminology), BCP-47, and the legacy Macintosh numeric language code
(SH-09).

This is **not** a demuxer and registers no component — `vaco-demux-matroska`,
`vaco-demux-mp4`, `vaco-demux-mxf`, `vaco-demux-flv`/`asf`, `vaco-demux-mpegts`
and any subtitle format that carries a language tag call into this crate the
way a container calls into `vaco-format-riff` or `vaco-format-isom`.

---

## What it is

| Module | Contents |
|---|---|
| `table` | the ISO 639 table: 639-1, 639-2/B, 639-2/T, English name |
| `mac` | the legacy Macintosh numeric language code |

Written from the ISO 639-2 Registration Authority's own published code list
(Library of Congress) and ISO 639-1, plus Apple's *QuickTime File Format
Specification* for the Macintosh numeric table. No FFmpeg source was
consulted (D7/D15) — this is registration data (a code, a name, and, for
twenty languages, two three-letter spellings the registration authority
itself assigns), which is scenes-a-faire/merger under D9, not anyone's
creative expression.

**A working subset, not the full registry.** ISO 639-2 has roughly 480
entries including language families and historical languages; `table`
covers the ~130 languages and four special codes (`und`/`mul`/`zxx`/`mis`)
realistically found in media metadata. `mac` likewise covers the common
~90 Macintosh codes, not the specification's full ~150. Extending either is
adding a row — see *How to change it*.

---

## How it works

`parse` is the one entry point every consumer wants: given `eng`, `en`,
`en-US`, or the Macintosh numeric code `0`, it resolves all four to the same
`ResolvedLanguage`, because a real corpus spells one language four different ways
depending on which container wrote it (module docs on `lib.rs` have the
full table of who writes which spelling). Resolution order:

1. A bare non-negative integer → the legacy Macintosh table (`mac::to_639_2t`).
2. A two-letter code → ISO 639-1 (`find_by_639_1`, case-insensitive).
3. A three-letter code → ISO 639-2, matching *either* the bibliographic or
   the terminology spelling (`find_by_639_2`) — so `ger` and `deu` both
   resolve to the same German entry.
4. A three-letter code in the private-use range `qaa`–`qtz` → passed through
   unchanged (lower-cased), since the specification assigns it no fixed
   meaning and it is not a name to synthesise a table entry for.
5. Anything containing `-` → a BCP-47 tag: the primary subtag is resolved by
   one of the rules above, and everything after the first `-` is kept as
   `ResolvedLanguage::region` (upper-cased when it is a plain two-letter region;
   passed through verbatim otherwise — full BCP-47 script/variant/extension
   grammar is out of scope, see below).

`to_639_1`/`to_639_2b`/`to_639_2t` are the narrower single-field
conversions, built on `parse` so they accept every spelling it does.

### What this crate does not attempt

Full BCP-47 (RFC 5646) — script subtags, extended language subtags,
variants, extensions, private-use tags in their full generality — is a much
larger grammar than any container in this workspace actually reads back.
`parse` extracts the primary language subtag and keeps the remainder
verbatim as `region` rather than parsing it further; nothing downstream
depends on the shortcut, so extending it later is additive.

---

## How to change it

- **Add a language to `table::LANGUAGES`**: one `lang!("xx", "xyz",
  "Name")` row (or the four-argument form for a bibliographic/terminology
  pair). No other code changes — every lookup function scans the table.
- **Add a Macintosh code to `mac::MAC_LANGUAGE_TO_639_2T`**: one `(code,
  "iso")` tuple.
- **Full BCP-47 parsing**: extend `parse`'s region branch (currently a
  single `split_once('-')`) rather than replacing it; the primary-subtag
  resolution rules above should stay exactly as they are for the common
  case.

## Configuration

None. Every function is a pure, allocation-free (aside from the owned
`String` a couple of conversions return) table lookup.

## Dependencies

None beyond the standard library. This crate has no `[dependencies]` at all
— tables and string matching need nothing else.
