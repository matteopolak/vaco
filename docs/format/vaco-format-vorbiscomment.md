# `vaco-format-vorbiscomment`

## What it is

Vorbis comment (vendor + tag list) parsing and FLAC `METADATA_BLOCK_PICTURE`
parsing. Vorbis, FLAC and Opus all carry the same vendor-plus-tag-list
metadata shape; this crate is the shared home for it that work packages
`#274` (Vorbis/FLAC bitstream headers) and `#540` (this package) both need,
so it exists once rather than twice.

## How it works

| Module | Contents |
|---|---|
| `comment` | `VorbisComment` — `parse_raw` (the bare vendor+list, what a FLAC `VORBIS_COMMENT` block *is*) and `parse_native` (`\x03vorbis` magic + the same content + a framing byte, what a native Vorbis comment header packet is) |
| `picture` | `Picture` — FLAC's `METADATA_BLOCK_PICTURE`, plus `PictureType`'s standard `0..=20` enumeration |
| `conv` | `TABLE` — the measured Vorbis-comment field-name renames |

Every field layout is verified against real bytes a real encoder wrote
(`ffmpeg -c:a vorbis`/`-c:a flac`, inspected with a throwaway Python script),
not transcribed from specification prose alone — see the `Vaco-Spec-Ref`
trailers on the commit and the module docs for exactly what was measured.

### The `conv` table only lists genuine renames

Writing each canonical `vaco-format-metadata::keys` key with `-metadata` into
a real FLAC file and reading its `VORBIS_COMMENT` block back showed most keys
round-trip **unchanged**, lower-case: `title`, `artist`, `album`, `date`,
`genre`, `comment`, `composer`, `copyright`, `encoder`, `description`,
`performer`, `publisher`. Three are genuinely renamed to the
community-standard spelling: `track` → `TRACKNUMBER`, `disc` →
`DISCNUMBER`, `album_artist` → `ALBUMARTIST`. `TABLE` lists only those three
— an unmapped key already passes through unchanged via
`MetadataConv::map_key`, which is the *correct* behaviour for the rest, not
merely the default one.

### What is deliberately not shared

`vaco-parse-opus::comment::CommentHeader` parses the identical wire shape for
`OpusTags` and predates this crate. It is not refactored to depend on this
one — that crate is owned by a different work package and editing it is out
of this crate's scope. Recorded as a known, accepted duplication.

## How to change it

A container-specific rename table (ID3v2, QuickTime `©xxx`) does **not**
belong here — see `vaco-format-metadata`'s own doc for why each container
ships its own `MetadataConv` table.

## Configuration

None.

## Dependencies

`vaco-core`, `vaco-format-metadata` (for the canonical key constants and the
`MetadataConv` driver `conv::TABLE` uses).

Fuzzed: `parse_vorbiscomment` covers both `VorbisComment::{parse_raw,parse_native}`
and `Picture::parse`.
