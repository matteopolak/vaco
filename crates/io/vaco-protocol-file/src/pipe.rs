//! The `pipe:` protocol.
//!
//! # A limit worth stating plainly
//!
//! `pipe:<n>` for an arbitrary descriptor, and plan 18's separate `fd:`
//! protocol, both require turning an integer into an owned descriptor. The only
//! way to do that in Rust is `FromRawFd::from_raw_fd`, which is `unsafe` — and
//! justifiably so: nothing proves the integer names a descriptor this process
//! owns, so a wrong value closes somebody else's socket on drop.
//!
//! D2 forbids `unsafe` outside `vaco-hw-*`, so **`pipe:0`, `pipe:1` and
//! `pipe:2` are supported and nothing else is.** They are reachable through
//! `std::io::stdin`/`stdout`/`stderr`, which own their descriptors already.
//! Anything else is [`ProtocolError::Unsupported`] with that reason, rather
//! than a silently wrong open.

use vaco_io::{MediaSink, MediaSource, PeekSource, ReaderSource, WriterSink};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// The `pipe:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct PipeProtocol;

/// Which standard stream a `pipe:` URL names.
fn descriptor(url: &Url, default_fd: u32) -> Result<u32> {
    let rest = url.rest.trim();
    if rest.is_empty() {
        return Ok(default_fd);
    }
    rest.parse::<u32>().map_err(|_| ProtocolError::Malformed {
        scheme: "pipe",
        detail: "expected pipe:, pipe:0, pipe:1 or pipe:2",
    })
}

impl Protocol for PipeProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        match descriptor(url, 0)? {
            0 => Ok(Box::new(PeekSource::new(ReaderSource::new(
                std::io::stdin(),
            )))),
            _ => Err(ProtocolError::Unsupported {
                scheme: "pipe",
                operation: "reading a descriptor other than 0 (needs unsafe; see the module docs)",
            }),
        }
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        match descriptor(url, 1)? {
            1 => Ok(Box::new(WriterSink::new(std::io::stdout()))),
            2 => Ok(Box::new(WriterSink::new(std::io::stderr()))),
            _ => Err(ProtocolError::Unsupported {
                scheme: "pipe",
                operation: "writing a descriptor other than 1 or 2 (needs unsafe; see the module docs)",
            }),
        }
    }
}

/// The registry entry for `pipe:`.
pub static PIPE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "pipe",
    long_name: "standard input/output",
    flags: ProtocolFlags::LOCAL,
    default_whitelist: &[],
    options: None,
    proto: &PipeProtocol,
};
