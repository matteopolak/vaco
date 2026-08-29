# `vaco-mux-whip`

Layer 4. The `whip` muxer — WHIP (WebRTC-HTTP Ingestion Protocol,
`draft-ietf-wish-whip`) publish. Closes #619.

## What it is

Turns a media stream into SRTP-protected RTP and sends it to a WHIP
endpoint: `POST` an SDP offer over HTTP, receive an SDP answer, run an ICE
connectivity check, complete a DTLS handshake, derive SRTP keys, then
packetise and send. It is the first muxer in this tree that needs network
*negotiation* before any byte-oriented sink exists.

## The extension point (#619's real question)

Every `Muxer` is opened as `MuxerDesc::open: fn(Box<dyn MediaSink>) ->
Result<Box<dyn Muxer>>` — a pre-connected byte sink. WHIP cannot fit that
shape: HTTP POST/answer, ICE, and DTLS all have to happen before there is
anything resembling a byte sink. **No change to that signature, or to the
`Muxer` trait, was needed.** The pieces already existed in
`vaco-format-core`, just never connected for this purpose:

1. WHIP is `AVFMT_NOFILE` — measured directly against `ffmpeg 9.0.1`
   (`ffmpeg -f whip /this/is/not/a/url` never attempts to open that string
   as a file; it reaches the muxer's own dispatch and rejects `file` by
   name). `vaco-format-core` already models this as `FormatFlags::NOFILE`.
2. `Muxer::bind_url` (added earlier for `image2`'s `NEEDNUMBER` case) hands
   a muxer the real destination URL string once known. `WhipMuxer::open`
   ignores its `Box<dyn MediaSink>` entirely (as every `NOFILE` muxer
   already does) and `bind_url` just stores the endpoint.
3. `Muxer::init` — called once every stream is declared, before the header
   — is exactly early enough to build the SDP offer (which needs every
   stream's codec) and exactly the point to run the whole negotiation.
   `write_header` is then a no-op; `write_packet` packetises and sends.

The one change made *alongside* this crate: `vaco-format-core::Muxer::bind_url`'s
doc comment now names this pattern explicitly (a doc-only, additive change —
see that method's doc), and `vaco-cli::open_output`'s `NOFILE` branch now
also tries `bind_url`, exactly the way its `NEEDNUMBER` branch already did,
treating the default `Error::Unsupported` as "this muxer has no use for the
URL" (silently true for `null`/`mkvtimestamp_v2`, unchanged). No WHIP-specific
branch exists in the CLI.

## How it works

| Module | Job |
|---|---|
| `sdp` | Building the SDP offer; reading a WHIP answer's ICE credentials, DTLS fingerprint, `setup`, and candidate lines. |
| `candidate` | RFC 8839 `a=candidate` parsing (host/srflx, UDP only). |
| `http` | A minimal one-shot HTTP/1.1 client (`http://` only) for the WHIP `POST`/`DELETE` exchange — see its own docs for why it does not just call `vaco-protocol-http` (that crate owns `ureq`, D11). |
| `muxer` | `WhipMuxer` and `negotiate`, the orchestration. |

`negotiate` (in `muxer.rs`) is the whole flow: build offer → `POST` →
parse answer → try each candidate's ICE connectivity check → DTLS handshake
(client role: `setup:active` in our offer, confirmed against `mediamtx` to
elicit `setup:passive`) → verify the peer's real certificate fingerprint
against the SDP answer's `a=fingerprint` (never skipped) → export SRTP
keying material (RFC 5764 §4.2) → derive session keys → one `SrtpContext`
per declared stream, sharing the derived keys but each keyed by its own
SSRC.

## Two real bugs this crate's own interop pass found and fixed elsewhere

Neither is a WHIP-specific bug; both were pre-existing defects in shared
crates that a real, independent peer (`mediamtx` 1.20.1) exposed. Recorded
here because this is the only place their full story is; `git log`/the
crates' own doc comments have the terse version.

1. **`vaco-protocol-srtp`'s key derivation was wrong, twice.**
   `kdf::derivation_counter_block` XORed the label byte into index 0, then
   (after a first fix) index 8; the correct position, confirmed against
   `libsrtp` itself via its `pylibsrtp` binding, is index 7 (RFC 3711
   §4.3.1's `key_id` is 7 octets, not 6 — `r` is defined to have the same
   bit-length as the 48-bit packet index). Both wrong versions were
   internally self-consistent and passed every test that crate had; the
   real DTLS handshake and the real key export both worked, and every
   resulting SRTP packet was still silently dropped by `mediamtx`. Fixed in
   `vaco-protocol-srtp`, with a permanent known-answer test cross-checked
   against `libsrtp`.
2. **AVCC vs. Annex-B.** `vaco-mux-rtp`'s H.264 RTP packetiser needs Annex-B
   NAL units; a stream copied straight out of an MP4 demuxer is
   length-prefixed (`AVCC`). Without a bitstream filter, the packetiser
   silently produced zero RTP payloads per access unit while `MuxReport`
   still counted the input bytes as "written" — no error, no packets,
   nothing on the wire. `WhipMuxer::check_bitstream` inserts
   `h264_mp4toannexb` when `CodecParameters::video::nal_length_size`
   (or, failing that, a start-code sniff) says the input is not already
   Annex-B, using the same `bsf_decided` per-stream guard `vaco-mux-mp4`
   already uses for the identical re-ask problem.

## `mediamtx` runs full ICE, not ICE-lite — measured, and it mattered

Assumed at first (most WHIP servers publish reachable host candidates
directly and never issue their own checks) and wrong for this real peer:
`mediamtx` 1.20.1 keeps sending Binding Requests to the publisher
throughout the DTLS handshake window, signed with *our* local ICE password.
`vaco-protocol-ice::respond_to_binding_request` answers them; without it,
the peer's own connectivity never confirms and DTLS never receives a
reply — total silence, which reads exactly like "the handshake failed" with
no clue why until traced with a byte-logging instrumented DTLS client
against the real server. `muxer::DemuxTransport` is the `Read`/`Write`
shim that demultiplexes STUN from DTLS on the one shared UDP socket while
the handshake is in progress, built on a small additive change to
`vaco-protocol-dtls` (`connect::handshake_over`, generic over the
transport — `connect::handshake` is now a thin wrapper over it).

## What is deliberately not implemented

- `https://` WHIP endpoints (media is always DTLS/SRTP-encrypted
  regardless of whether the signalling HTTP itself is TLS-wrapped).
- Trickle ICE (`PATCH` with `application/trickle-ice-sdpfrag`) and
  server-reflexive/relay (TURN) candidates — every peer measured so far
  publishes reachable host candidates directly, non-trickled, matching real
  `ffmpeg 9.0.1`'s own WHIP client (captured directly off the wire).
- RFC 7675 consent-freshness responses once the session is established
  (only the handshake-window responder above is built).
- RTCP receiver reports (this crate is send-only, `a=sendonly`).
- More than one audio/video stream sharing one SDP `BUNDLE` group is
  architecturally supported (one `SrtpContext` per stream, one shared
  transport) but only a single H.264 video stream has been verified this
  pass.

## Configuration

No `-h muxer=whip` option surface yet (`ffmpeg -h muxer=whip` has one —
`-handshake_timeout`, `-pkt_size`, `-authorization`, `-cert_file`/`-key_file`,
`-whip_flags dtls_active` — none implemented here). Fixed internal
constants instead: `DEFAULT_MTU = 1200` (matches the reference's own
`-pkt_size` default), `ICE_TIMEOUT`/`ICE_RETRIES`, `DTLS_HANDSHAKE_TIMEOUT`,
`HTTP_TIMEOUT` (all in `muxer.rs`).

## Dependencies

`vaco-protocol-dtls` (DTLS, transitively `openssl` — this crate declares
neither directly, only calls the public functions), `vaco-protocol-srtp`,
`vaco-protocol-ice`, `vaco-protocol-socket` (UDP dial), `vaco-protocol-http`
(only for `url::resolve_location`, a pure-string function — not `ureq`),
`vaco-mux-rtp` (H.264/Opus packetisers), `vaco-format-rtp` (`RtpHeader`,
SDP parsing), `vaco-hash` (SHA-256 for the fingerprint check).

Native-only: `xtask/src/wasm.rs`'s `NATIVE_ONLY` list carries this crate
(transitively via `vaco-protocol-dtls`/`vaco-protocol-socket`).
