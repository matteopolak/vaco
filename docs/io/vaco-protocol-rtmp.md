# vaco-protocol-rtmp

## What it is

The RTMP chunk stream layer (handshake, chunking/dechunking, control
messages, #552/PR-09a), AMF0 and the NetConnection/NetStream command flow
(#553/PR-09b), and the `rtmps`/tunnelled-variant findings (#554/PR-09c) —
all three PR-09 packages land in this one crate; epic #61's split is
single-writer ownership across issues, not across crates. There is still
no `rtmp:`/`rtmps:` `Protocol` implementation and no registry entry: that
needs socket ownership this crate does not have.

## How it works

- `handshake` — `build_plain_c0_c1`/`build_plain_c2` for the byte exchange
  Adobe's own specification documents; `build_digest_c0_c1`/
  `build_digest_c2`/`find_digest`/`verify_digest_s2` for the HMAC-SHA256
  digest scheme real deployments actually negotiate, which Adobe's
  specification does not document at all. See that module's own docs for
  the two independent sources this was cross-checked against, and exactly
  what remains unverified.
- `chunk` — stateless codecs for one chunk's basic header (1/2/3-byte forms)
  and message header (the four `fmt` shapes), plus the extended-timestamp
  sentinel.
- `message` — `Dechunker` holds one internal state per chunk stream ID
  (needed for `Type1`/`Type2`/`Type3` delta compression and for reassembling
  a message split across several chunks) and turns a byte stream into whole
  `RtmpMessage`s; `chunk_message` does the reverse.
- `control` — encode/decode for the six protocol control messages (Set
  Chunk Size, Abort, Acknowledgement, User Control, Window Acknowledgement
  Size, Set Peer Bandwidth) and the named `UserControlEvent` sub-types.
- `crypto` — hand-rolled SHA-256/HMAC-SHA256; see that module's docs for why
  this is not the `sha2`/`hmac` crates.
- `amf0` (#553) — AMF0 value encode/decode: Number, Boolean, String/Long
  String, Object, Null, Undefined, ECMA Array, Strict Array, Date. Six
  types are deliberately not built (Reference, MovieClip, RecordSet, XML
  Document, Typed Object, the AMF3-switch marker) — see the module's own
  docs for why none of them is needed by this crate's command flow.
- `command` (#553) — `Command` (name, transaction ID, command object,
  arguments) on top of `amf0`, plus builders/parsers for the named
  NetConnection/NetStream commands: `connect`/`connect_result`,
  `create_stream`/`create_stream_result`, `publish`/`on_status_publish_start`,
  `play`/`on_status_play_start` — each response using §7.2's own named
  status code (`NetConnection.Connect.Success`,
  `NetStream.Publish.Start`, `NetStream.Play.Start`).

## How to change it

Adding AMF/NetConnection on top (PR-09b): build a session type that owns a
`Dechunker` and a raw duplex stream (reached through `vaco-protocol-dial`,
once this crate takes that dependency), drives `handshake::build_*` at
connection time, and calls `control::ControlMessage::decode` on every
message with `message_type_id` 1-6 arriving on chunk stream 2 before
handing anything else to the AMF layer. `Dechunker::set_chunk_size` is
exactly what a received `Set Chunk Size` control message should call.

**`cargo xtask dead-code` will flag several public items here as orphans**
(`build_digest_c0_c1`, `build_digest_c2`, `build_plain_c2`,
`control::ControlMessage::encode_payload`, `handshake::plain_s2_echoes_our_c1`,
`message::Dechunker::set_chunk_size`) — every one of them is exercised by
this crate's own tests and, where it makes sense to fuzz a generator at all,
its fuzz targets, but none has a caller outside this crate yet. That is
expected for a framing library with no session type built on it: PR-09b is
the change that calls them, at connection-setup time and on every received
`Set Chunk Size`.

## Configuration

None — no `Protocol`, no `-h protocol=rtmp` options (yet).

## Dependencies

`vaco-protocol-core` for `Result`/`ProtocolError`. `vaco-limits` for
`Budget`/`IncrementalVec`, used in `message::Dechunker` to size a message
body from RTMP's 3-byte (up to ~16 MiB), peer-controlled length field
without ever allocating the declared size up front. `vaco-time` to seed the
handshake's filler bytes on a wasm-safe clock. No `sha2`/`hmac` (see
`crypto`'s module docs) and no `rand` (see `rng`'s).

## Testing and what is unverified against a real server

Every module has unit tests, and `chunk`/`handshake` also have `proptest`
round-trip properties. Three fuzz targets cover the parsers that take
untrusted bytes directly: `protocol_rtmp_dechunk` (`Dechunker::feed` on an
arbitrary byte stream, split at an arbitrary point across two `feed` calls),
`protocol_rtmp_handshake_digest` (`find_digest` on an arbitrary 1536-byte
buffer), `protocol_rtmp_control` (`ControlMessage::decode` on an arbitrary
type/payload, then re-encode/re-decode). 15-30 second breadth runs on each:
exit 0, `fuzz/artifacts` empty, 1.1M/24.8M/17.5M executions respectively
(`protocol_rtmp_handshake_digest`'s coverage plateaus almost immediately —
expected for a fixed 1536-byte input whose two code paths are both
short HMAC computations, not a sign the harness stopped exploring).

**What is not verified, and cannot be from here**:

- **Nothing in this crate has completed a handshake against a real RTMP
  server.** There is no live server reachable in this environment. Every
  test either round-trips this crate's own encoder against its own decoder,
  or checks a sub-component (HMAC-SHA256) against a published standard's
  test vectors (FIPS 180-4, RFC 4231).
- **The digest handshake's offset formula and key constants** are
  implemented from an independent clean-room write-up (not Adobe's own
  specification, which omits this scheme entirely), cross-checked against a
  second independent source for the offset moduli and the 32-byte
  `RANDOM_CRUD` constant — the two agreed byte-for-byte, which is real
  corroboration, but neither source is Adobe, and neither is a real server.
  If a real server rejects this handshake, recheck the offset formula and
  the key constants first.
- **`Type3`'s extended-timestamp inheritance** (`message.rs`'s
  `extended_timestamp_in_effect`) is implemented per the specification's
  literal wording, but this is a widely reported real-world interop wrinkle
  — some deployed implementations have disagreed about whether a `Type3`
  chunk repeats the 4-byte extended timestamp field. Unverified against
  anything but this crate's own encoder.
- **C1's `time`/version fields** are filled with placeholder values (zero
  for the plain handshake, an arbitrary nonzero constant for the digest
  one) rather than a real client's actual version string — cosmetic per
  every source read for this crate, but not measured against what a real
  server does with an unrecognised version.
