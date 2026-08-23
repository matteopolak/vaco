# `vaco-bsf-core`

Layer 4. Shared plumbing for every `vaco-bsf-*` crate: the
`BitstreamFilter` driver and the registry-facing descriptor type. Issues
#349/#350.

---

## What it is

`vaco_codec_core::BitstreamFilter` is a hand-rolled push/pull state machine
(`send_packet`/`receive_packet`, the same shape as `Decoder`). Every filter in
`vaco-bsf-generic` and `vaco-bsf-h2645` needs the same boilerplate around it: a
bounded output queue, end-of-stream bookkeeping, and the
`Err(NeedMoreInput)`/`Err(Eof)` convention
`vaco_format_core::mux::BsfChain::filter`'s `drain_filter` helper relies on.
This crate writes that once.

| Item | What it does |
|---|---|
| `PacketMap` | The trait a filter author implements: "given this packet (or `None` for EOS), what packets come out" |
| `MappedFilter<T>` | Wraps a `PacketMap` into a full `BitstreamFilter` |
| `BsfDesc` | Static descriptor (`name`, `long_name`, `build: fn(&CodecParameters) -> Result<Box<dyn BitstreamFilter>>`) — the registry-facing analogue of `DecoderDesc`/`ParserDesc` |
| `MAX_QUEUED_PACKETS` | Cap on one filter instance's own output queue |

## How it works

A filter author writes a struct implementing `PacketMap::push`, which appends
zero or more output packets to a `VecDeque` for each input packet (or `None`
for end of stream). `MappedFilter::new(my_filter)` turns that into a
`BitstreamFilter`: `send_packet` runs `push` and enforces `MAX_QUEUED_PACKETS`;
`receive_packet` pops from the queue, answering `NeedMoreInput` when empty and
not yet at EOS, `Eof` when empty and at EOS.

`PacketMap` is deliberately narrower than `BitstreamFilter`: it has no
`Err(OutputPending)` escape, because `BsfChain::filter` never handles one — it
calls `send_packet` and immediately drains, propagating anything else as a
hard error. A `PacketMap` therefore always accepts its input.

### Why `MAX_QUEUED_PACKETS` is not `vaco_format_core::mux::MAX_BSF_EXPANSION`

`MAX_BSF_EXPANSION` bounds a whole **chain's** output per input packet,
enforced by the driver (`vaco-format-core`) that owns the chain. That driver
is not the only caller: a fuzz target drives a `Box<dyn BitstreamFilter>`
directly, with no chain and no chain-level cap above it. `MAX_QUEUED_PACKETS`
is the same idea, moved one layer down, so a single filter instance fed
pathological input cannot grow its own queue without bound even when nothing
above it is watching.

## How to change it

Add a filter by implementing `PacketMap` in `vaco-bsf-generic` or
`vaco-bsf-h2645`, wrapping it in `MappedFilter::new`, and exporting a
`BsfDesc`. Nothing here should need to change for a new filter — if it does,
the filter needs something `PacketMap` cannot express; see
`planning/INTERFACE-GAPS.md` for the one already recorded (no per-instance
option string reaches `BsfProvider::open`).

## Configuration

None.

## Dependencies

`vaco-codec-core` for `BitstreamFilter`/`CodecParameters`; `vaco-packet` for
`Packet`; `vaco-core`/`vaco-limits` for the error and budget types every
filter needs.
