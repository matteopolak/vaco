# `vaco-bsf-subtitle`

Layer 4. Subtitle bitstream filters. Issue #354 (B-06).

---

## What it is

`mov2textsub` and `text2movsub`: the pair that lifts plain UTF-8 text out of,
and wraps it back into, an MP4/QuickTime `mov_text` sample (ISO/IEC
14496-17's Text Sample format — a two-byte big-endian length followed by that
many bytes of text, optionally followed by style/box atoms). Unlike most of
this filter family, both are **real byte-level transforms**, not the
bare-name identity most `*_metadata`/`*_2*` filters turn out to be.

Measured end to end against `ffmpeg 8.1` (SRT -> `mov_text` -> MP4 -> filter,
and the reverse):

- `mov2textsub` strips the two-byte length prefix **and** any trailing style
  box, truncating to the declared length rather than passing everything
  after byte 2 through (confirmed with a `<b>Bold text</b>` cue whose
  `mov_text` packet carries a 22-byte `styl` box after the text).
- `text2movsub` prepends the two-byte big-endian length of the input, no
  other framing.
- The length prefix cannot express more than 65535 bytes: a 65535-byte input
  is accepted, a 65536-byte input is **refused outright** by the reference
  ("Invalid data found when processing input"), not truncated or wrapped.
  `text2movsub` reproduces the refusal — see `CONFORMANCE-FINDINGS.md`
  finding 31.

## What was measured and left out

| Filter | Why it is not here |
|---|---|
| `pgs_frame_merge` | No PGS encoder in this `ffmpeg` build and no PGS sample available — no fragmented input to measure the merge rule against, and the filter has no options to fall back on either. |
| `eia608_to_smpte436m` | Its output type, `smpte_436m_anc`, has no `CodecId` in this workspace — the filter would produce a stream typed as a codec nothing here can consume. |
| `smpte436m_to_eia608` | Its input type, `smpte_436m_anc`, has no `CodecId` either — unreachable outright, the same shape as `vaco-bsf-audio`'s `ahx_to_mp2`. |

## How it works

Same shape as every `vaco-bsf-*` crate: one `BsfDesc` per module on
`PacketMap`/`MappedFilter`. Neither filter restricts the codec at
construction (`ffmpeg -h bsf=mov2textsub`/`text2movsub` state no `Supported
codecs:` line, so none is invented here).

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`,
and add a `[[component]]` table to `vaco-component.toml`.

## Configuration

Neither filter has an `AVOption` in the reference, so gap 12
 does not limit anything here. `text2movsub`
enforces one measured bound (`u16::MAX` bytes of text) — see its own module
docs and `CONFORMANCE-FINDINGS.md` finding 31.

## Dependencies

`vaco-bsf-core` for the driver; `vaco-core`/`vaco-limits`/`vaco-packet` for
the error, budget and packet types. No codec-specific parsing crate — both
filters operate on the packet payload directly per ISO/IEC 14496-17.
