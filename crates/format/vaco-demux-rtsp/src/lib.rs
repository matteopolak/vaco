//! The RTSP 1.0/2.0 session layer, plus the `rtp`/`sdp` container demuxers.
//!
//! # What it is
//!
//! Layer 4 (`crates/format/`). RFC 2326 (RTSP 1.0 — what real cameras and
//! `ffmpeg`'s own reference implementation actually speak on the wire) and
//! RFC 7826 (RTSP 2.0, the same request/response grammar with a handful of
//! header and status-code additions): `OPTIONS`/`DESCRIBE`/`SETUP`/`PLAY`/
//! `PAUSE`/`TEARDOWN`/`GET_PARAMETER`, session ids, keepalive, and the four
//! transport modes an RTSP `Transport:` header can negotiate — UDP unicast,
//! TCP-interleaved, UDP multicast, and HTTP tunnelling.
//!
//! # The security posture — read this before touching [`transport`] or [`connection`]
//!
//! RTSP's whole job is negotiating a transport **with a remote server**, and
//! the server — not the caller — names the address and port that transport
//! then opens (`Transport:` response headers name `client_port=`/
//! `server_port=`, and a multicast `Transport:` names the group address
//! itself). A hostile or merely compromised RTSP server is exactly a remote
//! attacker who gets to choose those values, so this is the one place in
//! the whole `rtp`/`rtsp`/`sdp` trio where "just open what the server said"
//! would be a real vulnerability.
//!
//! **What a server-supplied transport address is allowed to be, concretely:**
//!
//! * **UDP unicast**: the RTP/RTCP *destination* port pair this crate sends
//!   receiver reports to is whatever `server_port=` the `SETUP` response
//!   names — RFC 2326 gives the server no other way to say where its RTP
//!   originates — but the *local* ports this crate binds to receive on are
//!   always chosen by this crate itself, from `-min_port`/`-max_port`
//!   ([`options::RtspOptions`], default `5000`/`65000`, matching
//!   `ffmpeg -h demuxer=rtsp` exactly). A server cannot make this crate bind
//!   a port it did not choose.
//! * **UDP multicast**: the server *does* name the group address and TTL
//!   directly (`Transport: ...;multicast;destination=<addr>;ttl=<n>`) — RFC
//!   2326 §C.1.1 defines multicast SETUP exactly this way, so joining
//!   whatever group the server names is the negotiation working as
//!   specified, not a bypass. The control that actually matters here is not
//!   "is this address allowed" (any multicast group is a legitimate
//!   destination) but "is `udp` on the whitelist at all" — see below.
//! * **TCP-interleaved**: no new socket is opened at all — RTP/RTCP travel
//!   as `$`-framed chunks over the *same* TCP connection this crate already
//!   holds open for RTSP control messages, so there is nothing new for a
//!   server to redirect.
//! * **HTTP tunnelling**: same shape as TCP-interleaved, over two `http`/
//!   `https` connections this crate itself opened to the DESCRIBE URL's own
//!   host — a server cannot point either leg anywhere but back at itself.
//!
//! So the one genuine remote-address decision a hostile server makes is
//! "which UDP host:port does this crate's receiver-report `MediaSink` send
//! to, and which multicast group does it join" — both bounded to the `udp`
//! scheme, never to `tcp`/`file`/anything else, and both go through
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] exactly where a nested
//! `ProtocolRegistry::resolve` call would (see [`transport::udp`]'s module
//! docs for the exact call sites) — **never around it**. `-protocol_whitelist`
//! not naming `udp` refuses the SETUP outright, exactly as it refuses a
//! nested `tcp` open under `-protocol_whitelist tls` (measured against
//! `ffmpeg 8.1`, `docs/io/vaco-protocol-tls.md`).
//!
//! **What `-protocol_whitelist` defaults to for `rtsp`, measured**:
//!
//! ```text
//! $ ffmpeg -v debug -rtsp_transport udp -i rtsp://127.0.0.1:<port>/x -f null -
//! [rtsp @ ...] No default whitelist set
//! ```
//!
//! Same shape as every protocol in `vaco-protocol-socket`/`vaco-protocol-tls`
//! that opens a nested URL of its own: `rtsp` grants **nothing** by default.
//! An embedder that wants an RTSP `-i` to actually receive media over UDP
//! must name `udp` (and `tcp` for interleaved/control) on its whitelist
//! explicitly — there is no curated default grant the way `hls`'s is. This
//! is the same answer HLS and DASH needed and is recorded in
//! `docs/io/vaco-protocol-tls.md` as "worth having" for exactly this reason:
//! three format crates independently needed to know it, so probing it once
//! here confirms rather than duplicates that work.
//!
//! A finding along the way: `ffmpeg -h demuxer=rtp` does **not** enumerate
//! the per-codec depacketiser table the way this crate's sibling
//! `vaco-format-rtp` needed for FM-41's count — see that crate's
//! `depacket` module docs for the full transcript and what was measured
//! instead.
//!
//! # What is in here
//!
//! | Module | Job |
//! |---|---|
//! | [`message`] | The RTSP request/response text grammar (RFC 2326 §4 / RFC 7826 §8) |
//! | [`transport`] | The `Transport:` header — parse, build, and the four modes |
//! | [`auth`] | `WWW-Authenticate`/`Authorization` — Basic and Digest (RFC 2617), MD5 via `vaco-hash` |
//! | [`base64`] | A small encoder/decoder — no `base64` crate is declared workspace-wide (D10), and HTTP tunnelling and Basic auth both need one |
//! | [`connection`] | The duplex control connection: raw `tcp`/`tls` connect (whitelist-checked by hand, mirroring `vaco-protocol-tls`), request/response round-trips, `$`-interleaved frame demuxing |
//! | [`transport::udp`] | Opens the RTP/RTCP UDP sockets a `SETUP` negotiated, through the registry |
//! | [`http_tunnel`] | RTSP-over-HTTP (Apple's tunnelling scheme): two HTTP legs, base64-framed |
//! | [`session`] | The state machine: `OPTIONS`/`DESCRIBE`/`SETUP`/`PLAY`/`PAUSE`/`TEARDOWN`/`GET_PARAMETER` keepalive |
//! | [`demux`] | [`demux::RtspDemuxer`] (`Demuxer` impl) and the registered `rtsp`/`rtp`/`sdp` [`vaco_format_core::DemuxerDesc`]s |
//! | [`options`] | [`options::RtspOptions`] — names and defaults from `ffmpeg -h demuxer=rtsp` (8.1), not memory |
//!
//! # A gap this crate reports rather than works around
//!
//! [`vaco_format_core::Demuxer`] has no `play`/`pause` methods — an earlier
//! planning draft sketched them for exactly this crate, but the trait as
//! actually frozen in `vaco-format-core` today does not have them (measured
//! by reading the real trait, not the plan). RTSP's `PAUSE` therefore has
//! nowhere to attach on the generic interface: [`demux::RtspDemuxer`]
//! exposes `pause`/`play` as its own inherent methods (reachable by a
//! caller that downcasts or holds the concrete type, exactly the shape
//! `HlsDemuxer::playlist()` already uses for its own not-in-the-trait
//! surface) and `Demuxer::seek` remains an ordinary time-domain seek for
//! callers that only have the trait object. This is the same "the trait
//! genuinely cannot express this yet" gap `vaco-protocol-tls` reports for
//! duplex transports, not a workaround.
//!
//! Similarly, [`vaco_format_core::DemuxerDesc::open`] takes exactly one
//! already-opened [`vaco_io::MediaSource`] and no URL, no protocol registry,
//! and no way to open a second connection — but an RTSP session **is**
//! opening the connection(s): there is no sensible "bytes already fetched"
//! to hand it the way an HLS playlist's bytes are. The registered `rtsp`
//! [`vaco_format_core::DemuxerDesc`] therefore cannot function at all
//! through that path and says so ([`demux::open_rtsp_desc`]) — the real
//! entry point a caller with network access must use is
//! [`demux::RtspDemuxer::open`], exactly parallel to `HlsDemuxer::open`'s
//! `access: Option<RemoteAccess>` parameter.

#![forbid(unsafe_code)]

pub mod auth;
pub mod base64;
pub mod connection;
pub mod demux;
pub mod http_tunnel;
pub mod message;
pub mod options;
pub mod session;
pub mod transport;

pub use demux::{RTP_DEMUXER, RTSP_DEMUXER, RtpDemuxer, RtspDemuxer, SDP_DEMUXER, SdpDemuxer};
pub use message::{Method, Request, Response};
pub use options::RtspOptions;
pub use transport::{TransportMode, TransportSpec};
