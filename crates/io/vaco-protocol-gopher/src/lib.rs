#![forbid(unsafe_code)]
//! `gopher:` and `gophers:` — RFC 1436, plus a TLS-wrapped variant with no
//! RFC of its own (the reference's own name for it — `-protocols` lists it
//! as `gophers` alongside `https`/`ftps`-shaped names, no options, no
//! documented spec).
//!
//! # What it is
//!
//! `gopher://host[:port]/<T><selector>` connects, sends `<selector>\r\n` (the
//! type character `<T>` is consumed by the client and never sent), and reads
//! (or, for `create`, writes) the raw bytes that follow — the whole exchange
//! is one request, one reply, no further framing. `gophers:` is identical
//! except the connection is TLS.
//!
//! # Measured against `ffmpeg 8.1`
//!
//! ## The type character gates what this protocol will fetch at all
//!
//! `gopher://host/1/menu` — item type `1`, a directory listing — fails
//! immediately after connecting: `Gopher protocol type '1' not supported
//! yet!`. Tried every RFC 1436 item type character against a local fake
//! server; only three succeed:
//!
//! | Type | Meaning | Accepted? |
//! |---|---|---|
//! | `0` (text), `1` (menu), `2`–`4`, `6`–`8`, `g`/`h`/`I`/`i`/`m`/`T`/`w` | text, directory, CSO, error, `BinHex`, `UUencoded`, index, telnet, GIF, HTML, image, inline text, DOS-history-style types, 3270, WHOIS+ | **No** |
//! | `5` | DOS binary archive | **Yes** |
//! | `9` | Binary file | **Yes** |
//! | `s` | Sound file | **Yes** |
//!
//! Every rejected type still opens the TCP connection first — the check
//! happens after connecting, before sending anything — so `check_type`
//! (below) never itself does I/O; it is called *between* connect and
//! send-selector.
//!
//! ## Selector parsing is one character, not one path segment
//!
//! `gopher://host/some/selector` (no `/` between a type character and the
//! rest — `some` is four characters) sends `/selector\r\n`, **not**
//! `ome/selector\r\n`. So the rule is not "strip the first path segment as
//! the type": it is "the first *character* of the first path segment is the
//! type, and everything else in that first segment is discarded", with the
//! selector then starting at the *next* `/` (inclusive) if one exists, or
//! empty otherwise. See [`selector::parse`] for the exact algorithm and its
//! test against this transcript.
//!
//! ## `default_whitelist` is genuinely non-empty — the first protocol
//! measured in this workspace where that is true
//!
//! ```text
//! $ ffmpeg -v debug -i "gopher://127.0.0.1:PORT/9/x" -f null -
//! [gopher @ ...] Setting default whitelist 'gopher,tcp'
//!
//! $ ffmpeg -v debug -i "gophers://127.0.0.1:PORT/9/x" -f null -
//! [gophers @ ...] Setting default whitelist 'gopher,gophers,tcp,tls'
//! ```
//!
//! Every other nested-opening protocol measured so far in this workspace
//! (`crypto`, `tls`, `httpproxy`, `ftp`) has an **empty** default grant; this
//! is the exception, matching a real design reason — a gopher item can be a
//! menu whose entries point at further gopher (or plain) resources, and the
//! reference pre-grants exactly the schemes a gopher session could
//! legitimately need next. An *explicit* `-protocol_whitelist gopher` (W3:
//! replaces rather than unions the default) still refuses the nested `tcp`
//! open, exactly like every other protocol's default grant under an
//! explicit whitelist.
//!
//! ## Output works the same way, selector first
//!
//! `create()` connects, sends the selector line, and every subsequent
//! `write` goes straight to the socket with no further framing — confirmed
//! against a raw-byte capture: `gopher://host/9/out` with muxed input
//! `hello output data` produced exactly `/out\r\nhello output data` on the
//! wire.
//!
//! # Security
//!
//! Selector round trip is inherently duplex (write the selector, then treat
//! the same connection as read-only or write-only depending on direction) —
//! `Protocol::open`/`create` each return one direction, so, like `tls:`,
//! `httpproxy:` and `ftp:` in this workspace, the connection is dialled
//! directly rather than through the registry, with `env.check_scheme`
//! called by hand for every scheme actually used (`"tcp"` for `gopher:`;
//! `"tls"` then `"tcp"` for `gophers:`, reusing
//! `vaco_protocol_tls::connect::{connect_tcp, handshake}` rather than
//! duplicating TLS handling).

pub mod protocol;
pub mod selector;

pub use protocol::{GOPHER_PROTOCOL, GOPHERS_PROTOCOL, GopherProtocol, GophersProtocol};
