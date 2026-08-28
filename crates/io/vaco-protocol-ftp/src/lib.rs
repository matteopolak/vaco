#![forbid(unsafe_code)]
//! `ftp:` — RFC 959 (control connection, login, PASV/EPSV) plus RFC 2428
//! (`EPSV`).
//!
//! # What it is
//!
//! `ftp://[user[:pass]@]host[:port]/path` logs in (anonymous by default),
//! probes seekability and size, opens a passive data connection, and
//! transfers `path` with `RETR` (read) or `STOR` (write). Everything about
//! *which* commands are sent, in what order, was measured against a local
//! fake FTP server — see [`control`]'s module docs and
//! `docs/io/vaco-protocol-ftp.md` for the full transcript — rather than
//! assumed from the RFC, because the RFC specifies what a compliant server
//! must accept, not what this specific client actually sends.
//!
//! # Measured command sequence
//!
//! Both directions, before the first byte of data:
//!
//! ```text
//! (connect)                          -> 220 <greeting>
//! USER <user>                        -> 331
//! PASS <password>                    -> 230
//! TYPE I                             -> 200
//! FEAT                               -> 211 (ignored beyond reading it)
//! PWD                                -> 257 (ignored beyond reading it)
//! REST 0                             -> 350
//! SIZE <path>                        -> 213 <n>
//! EPSV                               -> 229 (|||port|)      [PASV on failure]
//! RETR <path> / STOR <path>          -> 150, then the data connection
//! ```
//!
//! `<path>` is always `url.rest`'s full path, unmodified — measured: no
//! `CWD` is issued even for a path containing `/`, so this crate does not
//! implement directory-relative navigation via `CWD` at all (a scoping
//! decision recorded here, not an oversight: nothing measured needed it).
//!
//! # Login defaults
//!
//! `user` is the URL's userinfo, else `-ftp-user`, else `anonymous`.
//! `password` is the URL's userinfo, else `-ftp-password`, else — only when
//! `user == "anonymous"` — `-ftp-anonymous-password`, else the reference's
//! own measured default: the literal string `nopassword` (not an email
//! address, despite `-h protocol=ftp`'s help text suggesting one).
//!
//! # Security
//!
//! Both the control connection and each data connection call
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` by hand
//! before connecting — measured empty `default_whitelist`
//! (`-protocol_whitelist ftp` alone refuses the nested `tcp` open with
//! `Protocol 'tcp' not on whitelist 'ftp'!`).

pub mod control;
pub mod options;
pub mod protocol;
pub mod sink;
pub mod source;

pub use protocol::{FTP_PROTOCOL, FtpProtocol};
