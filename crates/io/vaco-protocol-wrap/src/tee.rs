//! `tee:` — write the same bytes to several outputs at once.
//!
//! # Grammar and scope
//!
//! `tee:out1.mkv|out2.mkv`, `|`-separated with no escaping — the same
//! convention measured for `concat:` (see that module's docs), and consistent
//! here too: `ffmpeg -f nut "tee:a.nut|b.nut"` produced two byte-identical
//! files.
//!
//! **This is the `tee:` *protocol*, not the `tee` *muxer*.** The reference has
//! both, and they are genuinely different things at different layers:
//!
//! * `-f tee "a|[f=mpegts]b"` selects the **muxer**, which can send each
//!   output through a *different* container format via the bracketed
//!   `[key=value]` per-output options shown in `vaco-protocol-core`'s own
//!   module-doc example.
//! * A plain muxer writing to `tee:a|b` (e.g. `-f nut "tee:a.nut|b.nut"`, which
//!   is exactly what was measured) uses the **protocol**: it duplicates raw
//!   bytes to every output verbatim, with no format re-interpretation at all.
//!
//! The muxer belongs in a `vaco-mux-tee` format crate, not here — this crate
//! owns protocols, and `vaco-protocol-core::Protocol` has no way to express
//! "re-encode this packet stream per output" in the first place. Bracketed
//! per-output options are therefore **not** parsed by this module; a segment
//! that starts with `[` is opened as a literal URL (and, like any URL
//! `open`/`create` cannot resolve, fails through the ordinary error path)
//! rather than silently ignored.
//!
//! # Security
//!
//! Every output is a nested open through the same [`ProtocolEnv`] this
//! protocol was given, same as `concat:`/`subfile:`. `default_whitelist` is
//! empty for the same measured reason (see the crate docs).

use vaco_io::MediaSink;
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// A [`MediaSink`] that fans one write out to several already-opened sinks.
pub struct TeeSink {
    sinks: Vec<Box<dyn MediaSink>>,
    pos: u64,
}

impl std::fmt::Debug for TeeSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeeSink")
            .field("outputs", &self.sinks.len())
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl TeeSink {
    /// Wrap already-opened `sinks`. Every write and seek goes to all of them,
    /// in order.
    #[must_use]
    pub const fn new(sinks: Vec<Box<dyn MediaSink>>) -> Self {
        Self { sinks, pos: 0 }
    }
}

impl MediaSink for TeeSink {
    fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
        // First failure wins and aborts the whole write. The reference's own
        // *muxer* tracks per-output failure and keeps going (its `-tee`
        // semantics are documented that way); the plain protocol this module
        // implements has no such bookkeeping to hang a "keep going" policy on,
        // so failing the caller's write is the honest answer rather than
        // silently dropping an output.
        for sink in &mut self.sinks {
            sink.write(buf)?;
        }
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        if !self.is_seekable() {
            return Err(vaco_core::Error::NotSeekable);
        }
        for sink in &mut self.sinks {
            sink.seek(pos)?;
        }
        self.pos = pos;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        // All or nothing: a seek that only some outputs can honour would
        // desynchronise them, which is worse than refusing it.
        !self.sinks.is_empty() && self.sinks.iter().all(|s| s.is_seekable())
    }

    fn flush(&mut self) -> vaco_core::Result<()> {
        for sink in &mut self.sinks {
            sink.flush()?;
        }
        Ok(())
    }
}

/// Split a `tee:` URL's `rest` on literal `|`. No escaping, matching
/// `concat:`'s measured grammar.
#[must_use]
pub fn split_outputs(rest: &str) -> Vec<&str> {
    rest.split('|').collect()
}

/// The `tee:` protocol. Output-only, per the reference's own `-protocols`
/// listing.
#[derive(Debug, Clone, Copy, Default)]
pub struct TeeProtocol;

impl Protocol for TeeProtocol {
    fn open(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn vaco_io::MediaSource>> {
        Err(ProtocolError::Unsupported {
            scheme: "tee",
            operation: "reading (tee: is an output-only protocol)",
        })
    }

    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        if url.rest.is_empty() {
            return Err(ProtocolError::Malformed {
                scheme: "tee",
                detail: "empty output list",
            });
        }
        let mut sinks = Vec::new();
        for out in split_outputs(&url.rest) {
            sinks.push(env.registry.create(out, flags, opts, env)?);
        }
        Ok(Box::new(TeeSink::new(sinks)))
    }
}

/// The registry entry for `tee:`.
pub static TEE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "tee",
    long_name: "Tee muxer",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: false,
        writable: true,
    },
    default_whitelist: &[],
    options: None,
    proto: &TeeProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]
mod tests {
    use super::*;
    use vaco_io::DynBuf;

    #[test]
    fn splits_on_literal_pipe() {
        assert_eq!(split_outputs("a|b|c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn writes_reach_every_sink_byte_identical() {
        let a = vaco_io::SharedDynBuf::new();
        let b = vaco_io::SharedDynBuf::new();
        let mut tee = TeeSink::new(vec![Box::new(a.clone()), Box::new(b.clone())]);
        tee.write(b"hello").unwrap();
        tee.write(b" world").unwrap();
        assert_eq!(a.snapshot(), b"hello world");
        assert_eq!(b.snapshot(), b"hello world");
    }

    #[test]
    fn a_failing_output_fails_the_whole_write() {
        struct AlwaysFails;
        impl MediaSink for AlwaysFails {
            fn write(&mut self, _: &[u8]) -> vaco_core::Result<()> {
                Err(vaco_core::Error::Io(std::io::Error::other("nope")))
            }
            fn seek(&mut self, _: u64) -> vaco_core::Result<u64> {
                Err(vaco_core::Error::NotSeekable)
            }
            fn position(&self) -> u64 {
                0
            }
            fn is_seekable(&self) -> bool {
                false
            }
            fn flush(&mut self) -> vaco_core::Result<()> {
                Ok(())
            }
        }
        let good = vaco_io::SharedDynBuf::new();
        let mut tee = TeeSink::new(vec![Box::new(good.clone()), Box::new(AlwaysFails)]);
        assert!(tee.write(b"x").is_err());
    }

    #[test]
    fn seekable_only_when_every_sink_is() {
        let seekable = DynBuf::new();
        let unseekable = vaco_io::WriterSink::new(Vec::<u8>::new());
        let tee = TeeSink::new(vec![Box::new(seekable), Box::new(unseekable)]);
        assert!(!tee.is_seekable());
    }

    #[test]
    fn create_opens_every_named_output() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");

        let mut registry = vaco_protocol_core::ProtocolRegistry::new();
        vaco_protocol_file::register(&mut registry);
        registry.register(&TEE_PROTOCOL);
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);

        let url = format!("tee:{}|{}", a.to_str().unwrap(), b.to_str().unwrap());
        let mut sink = registry
            .create(&url, IoFlags::WRITE, &Dict::new(), &env)
            .unwrap();
        sink.write(b"payload").unwrap();
        sink.flush().unwrap();
        drop(sink);

        assert_eq!(std::fs::read(&a).unwrap(), b"payload");
        assert_eq!(std::fs::read(&b).unwrap(), b"payload");
    }

    #[test]
    fn empty_output_list_is_malformed() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("tee:");
        assert!(matches!(
            TeeProtocol.create(&url, IoFlags::WRITE, &Dict::new(), &env),
            Err(ProtocolError::Malformed { scheme: "tee", .. })
        ));
    }
}
