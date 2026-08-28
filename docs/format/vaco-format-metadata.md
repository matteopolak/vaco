# `vaco-format-metadata`

## What it is

The canonical metadata key set, the `MetadataConv` table type and driver, and
the `StreamGroup` model — the shared parts of FW-12, split from what already
existed. It is a pure data/transform crate: nothing here reads bytes from a
file.

## How it works

| Module | Contents |
|---|---|
| `keys` | `&str` constants for the generic metadata keys `-metadata` accepts: `title`, `artist`, `album`, `album_artist`, `date`, `track`, `disc`, `genre`, `comment`, `composer`, `copyright`, `encoder`, `description`, `performer`, `publisher`, `language`. Measured with `ffprobe 8.1`, not exhaustive — add to it when a real need appears. |
| `conv` | `ConvEntry`/`MetadataConv`: a `(generic, native)` table, plus `to_native`/`to_generic`/`map_key`/`convert`. An unmapped key passes through unchanged, matching the reference. |
| `stream_group` | `StreamGroup`/`StreamGroupIndex`/`StreamGroupKind`/`TileGrid` — a named set of streams forming one logical unit, e.g. a HEIF/AVIF tiled grid image. |

### What is deliberately not here

`Program` and `Chapter` already exist in `vaco-format-core` (FW-01) and are
re-exported from this crate's root rather than redefined — every demuxer
already returns the real ones through `Demuxer::programs`/`chapters`, so a
second type here would be exactly the D19 mistake this project keeps a page
of postmortems about.

`StreamGroup` has no such prior definition: plan 18 §1.1 sketched one but it
never landed in `vaco-format-core`. It lives here using that crate's actual
field conventions (`u32` stream indices, `Vec<(String, String)>` metadata) —
not the plan's own sketch, which used types (`StreamIndex`, `Metadata`) that
were never built. **Nothing constructs one yet and `Demuxer` has no
`stream_groups()` method** — wiring that in is a `vaco-format-core` change,
out of this crate's ownership. A HEIF/AVIF `grid` item is the obvious first
producer.

### Each container ships its own `MetadataConv` table

This crate defines the table *type* and the *driver* that applies one; it
does not author a table for ID3v2, QuickTime, RIFF `INFO`, or Vorbis comment
field names. Those belong in the container crate that already reads or
writes them — `vaco-format-id3`, for instance, likely already has its own
ID3v2 frame-ID mapping, and duplicating it here under a second name is
exactly what this design avoids. A container crate wanting one imports this
crate, defines its own `&'static [ConvEntry]`, and calls
`MetadataConv::convert`.

## How to change it

Add a `keys` constant only once it is actually measured against a real
`ffprobe`/`ffmpeg` round trip — the module doc explains why guessing from
"looks consistent" is the wrong standard here. Add a `StreamGroupKind`
variant additively; the enum is `#[non_exhaustive]` for exactly that.

## Configuration

None.

## Dependencies

`vaco-core` (for `Disposition`), `vaco-format-core` (for the re-exported
`Program`/`Chapter`).
