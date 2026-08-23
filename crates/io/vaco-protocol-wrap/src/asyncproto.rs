//! `async:` — read an inner URL ahead of the caller, on a background thread.
//!
//! # Grammar
//!
//! `async:inner-url`, e.g. `async:http://host/path` or `async:file:clip.mkv`.
//! Measured against `ffmpeg 8.1`'s `-h protocol=async`: no options at all.
//!
//! # Why this file, and not `vaco-time`, owns the thread
//!
//! The brief this module was written against says background reading and
//! `vaco-time` "are the one door for the clock *and* for `thread::spawn`".
//! **That is not what `xtask/src/time_gate.rs` says**, and the gate is
//! authoritative here: its `FORBIDDEN` table maps `std::thread::spawn` to "a
//! driver the caller supplies (D18)", not to any `vaco_time` function, and
//! `vaco-time`'s own source has no spawn wrapper at all — only `sleep`,
//! `Instant` and `unix_nanos`. The worked example the gate's own docs point
//! to, `vaco-sched`'s `run_threaded`, spawns directly with
//! `std::thread::spawn` inside a function marked
//! `#[cfg(not(target_family = "wasm"))]`, immediately above a serial fallback
//! that is *not* `cfg`'d out. This module follows that precedent instead:
//! [`spawn_reader`] is the one `cfg`-gated function that touches
//! `std::thread::spawn`, and [`AsyncSource`] falls back to reading `inner`
//! synchronously — still correct, just not ahead of the caller — wherever
//! threads do not exist. Same API on every target, per D18; see
//! `cargo xtask time-gate`'s own module docs for why a `#[cfg]`-excluded item
//! is not a finding.
//!
//! # Design
//!
//! One worker thread owns `inner` outright and pushes it through a bounded
//! channel in fixed-size chunks — bounded so a slow reader cannot let the
//! worker buffer the whole (possibly unbounded) stream in memory, which is
//! the entire point of "read ahead" rather than "read all of it first". A
//! second, single-slot command channel carries seeks from the caller to the
//! worker: a seek discards whatever is in flight and tells the worker to
//! re-seek `inner` and resume filling from the new position.
//!
//! # Security
//!
//! One nested open, `rest`, through the same [`ProtocolEnv`] — see the crate
//! docs for the measured whitelist behaviour this follows.

use std::sync::mpsc;

use vaco_io::{MediaSource, Seekability};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolFlags, Result, Url,
};

/// Chunk size the worker reads in. Small enough that a seek does not have to
/// wait long for the in-flight read to land, large enough that per-chunk
/// overhead (a channel send) is not the bottleneck.
const CHUNK: usize = 64 * 1024;
/// Chunks the channel holds before the worker blocks on `send`. This is the
/// entire "how far ahead" knob; the reference exposes no equivalent option
/// (see the module docs), so it is a constant rather than a configured value.
const QUEUE_DEPTH: usize = 4;

/// One message from the worker to the foreground.
enum Msg {
    Chunk(Vec<u8>),
    Eof,
    Err(vaco_core::Error),
    /// Acknowledges a [`Cmd::Seek`], carrying what `inner.seek` returned.
    SeekDone(std::result::Result<u64, vaco_core::Error>),
}

/// One message from the foreground to the worker.
enum Cmd {
    Seek(u64),
}

/// The threaded backend: a worker owns `inner` and streams it over a channel.
///
/// `data_rx`/`cmd_tx` are `Option` purely so [`Threaded::drop`] can drop them
/// — disconnecting the worker's channels — *before* joining it. Everywhere
/// else they are `Some`: nothing outside `Drop` ever takes them, so
/// [`Threaded::data_rx`]/[`Threaded::cmd_tx`] unwrapping is not a real
/// failure mode, only a way to keep the "only `Drop` may see `None`"
/// invariant out of every call site.
#[cfg(not(target_family = "wasm"))]
struct Threaded {
    data_rx: Option<mpsc::Receiver<Msg>>,
    cmd_tx: Option<mpsc::SyncSender<Cmd>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_family = "wasm"))]
impl Threaded {
    #[allow(
        clippy::unwrap_used,
        reason = "None only during Drop, after which nothing reads this field again"
    )]
    fn data_rx(&self) -> &mpsc::Receiver<Msg> {
        self.data_rx.as_ref().unwrap()
    }

    #[allow(
        clippy::unwrap_used,
        reason = "None only during Drop, after which nothing reads this field again"
    )]
    fn cmd_tx(&self) -> &mpsc::SyncSender<Cmd> {
        self.cmd_tx.as_ref().unwrap()
    }
}

#[cfg(not(target_family = "wasm"))]
fn worker_loop(
    mut inner: Box<dyn MediaSource>,
    data_tx: &mpsc::SyncSender<Msg>,
    cmd_rx: &mpsc::Receiver<Cmd>,
) {
    loop {
        // A pending seek always takes priority over the read it interrupts:
        // draining it here rather than after the read keeps the worker from
        // ever sending a chunk from the position the caller already abandoned.
        match cmd_rx.try_recv() {
            Ok(Cmd::Seek(pos)) => {
                let result = inner.seek(pos);
                if data_tx.send(Msg::SeekDone(result)).is_err() {
                    return;
                }
                continue;
            }
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let mut buf = vec![0u8; CHUNK];
        match inner.read(&mut buf) {
            Ok(0) => {
                if data_tx.send(Msg::Eof).is_err() {
                    return;
                }
                // Keep serving commands after EOF: a seek can still revive
                // the stream.
                match cmd_rx.recv() {
                    Ok(Cmd::Seek(pos)) => {
                        let result = inner.seek(pos);
                        if data_tx.send(Msg::SeekDone(result)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            Ok(n) => {
                buf.truncate(n);
                if data_tx.send(Msg::Chunk(buf)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = data_tx.send(Msg::Err(e));
                return;
            }
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn spawn_reader(inner: Box<dyn MediaSource>) -> Threaded {
    let (data_tx, data_rx) = mpsc::sync_channel::<Msg>(QUEUE_DEPTH);
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<Cmd>(1);
    let worker = std::thread::spawn(move || worker_loop(inner, &data_tx, &cmd_rx));
    Threaded {
        data_rx: Some(data_rx),
        cmd_tx: Some(cmd_tx),
        worker: Some(worker),
    }
}

#[cfg(not(target_family = "wasm"))]
impl Drop for Threaded {
    fn drop(&mut self) {
        // Order matters: a worker blocked in `data_tx.send` (the channel is
        // at `QUEUE_DEPTH` and nothing is reading it any more, which is
        // exactly the state a caller that drops `AsyncSource` mid-stream
        // leaves it in) only unblocks once its receiver disconnects. Fields
        // are dropped in declaration order *after* this function returns, so
        // joining first — the original version of this code did — would wait
        // on a thread that cannot make progress until the very join it is
        // blocked on returns. Dropping the channel ends first breaks that
        // cycle: the worker's next `send`/`try_recv` sees "disconnected" and
        // exits, and only then does `join` have something to wait for that
        // will actually finish.
        self.data_rx.take();
        self.cmd_tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// One backend or the other, chosen once at construction and never mixed:
/// there is no scenario where a single `AsyncSource` needs both.
enum Backend {
    #[cfg(not(target_family = "wasm"))]
    Threaded(Threaded),
    /// No threads on this target: fall back to reading `inner` directly.
    /// Still correct — just not ahead of the caller. The only variant built
    /// on `wasm32`; on every other target it exists but [`AsyncSource::new`]
    /// never picks it, which is fine — see the module docs on why the same
    /// type is used on both.
    #[cfg_attr(
        not(target_family = "wasm"),
        allow(dead_code, reason = "only reachable on targets with no threads")
    )]
    Direct(Box<dyn MediaSource>),
}

/// `async:`'s [`MediaSource`]: reads `inner` ahead of the caller when this
/// target has threads, or reads it directly when it does not.
pub struct AsyncSource {
    backend: Backend,
    /// Bytes received from the worker but not yet handed to the caller.
    /// Unused by the `Direct` backend.
    leftover: Vec<u8>,
    leftover_pos: usize,
    pos: u64,
    eof: bool,
    inner_seekable: bool,
    inner_size: Option<u64>,
}

impl std::fmt::Debug for AsyncSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncSource")
            .field("pos", &self.pos)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl AsyncSource {
    /// Wrap `inner`, prefetching on a background thread wherever this target
    /// has one.
    #[must_use]
    pub fn new(inner: Box<dyn MediaSource>) -> Self {
        let inner_seekable = inner.seekability() != Seekability::None;
        let inner_size = inner.size();
        #[cfg(not(target_family = "wasm"))]
        let backend = Backend::Threaded(spawn_reader(inner));
        #[cfg(target_family = "wasm")]
        let backend = Backend::Direct(inner);
        Self {
            backend,
            leftover: Vec::new(),
            leftover_pos: 0,
            pos: 0,
            eof: false,
            inner_seekable,
            inner_size,
        }
    }

    fn leftover_slice(&self) -> &[u8] {
        self.leftover.get(self.leftover_pos..).unwrap_or(&[])
    }

    /// Compact `leftover` down to its unread tail, then append one more
    /// chunk. Compacting first keeps `leftover` from growing without bound
    /// across many small reads.
    fn append_leftover(&mut self, chunk: &[u8]) {
        if self.leftover_pos > 0 {
            self.leftover.drain(..self.leftover_pos);
            self.leftover_pos = 0;
        }
        self.leftover.extend_from_slice(chunk);
    }

    /// Pull from the backend, without moving `pos`, until `leftover` holds at
    /// least `want` unread bytes or the source reaches EOF.
    ///
    /// Shared by [`MediaSource::read`] and [`MediaSource::peek`] so neither
    /// has to know whether the bytes came from the worker channel or a direct
    /// call to `inner` — only [`AsyncSource::read`] moves `pos` afterward.
    fn top_up(&mut self, want: usize) -> vaco_core::Result<()> {
        while self.leftover_slice().len() < want && !self.eof {
            match &mut self.backend {
                Backend::Direct(inner) => {
                    let mut chunk = vec![0u8; want.clamp(1, CHUNK)];
                    let n = inner.read(&mut chunk)?;
                    if n == 0 {
                        self.eof = true;
                        break;
                    }
                    let Some(got) = chunk.get(..n) else { break };
                    self.append_leftover(got);
                }
                #[cfg(not(target_family = "wasm"))]
                Backend::Threaded(t) => match t.data_rx().recv() {
                    Ok(Msg::Chunk(chunk)) => self.append_leftover(&chunk),
                    Ok(Msg::Err(e)) => return Err(e),
                    // A stray `SeekDone` (no matching request) or a hung-up
                    // worker: either way there is nothing left to read.
                    Ok(Msg::Eof | Msg::SeekDone(_)) | Err(_) => self.eof = true,
                },
            }
        }
        Ok(())
    }
}

impl MediaSource for AsyncSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.top_up(buf.len())?;
        let have = self.leftover_slice();
        if have.is_empty() {
            return Ok(0);
        }
        let n = have.len().min(buf.len());
        let (Some(src), Some(dst)) = (have.get(..n), buf.get_mut(..n)) else {
            return Ok(0);
        };
        dst.copy_from_slice(src);
        self.leftover_pos += n;
        self.pos = self.pos.saturating_add(n as u64);
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        if !self.inner_seekable {
            return Err(vaco_core::Error::NotSeekable);
        }
        self.leftover.clear();
        self.leftover_pos = 0;
        self.eof = false;
        match &mut self.backend {
            Backend::Direct(inner) => {
                let at = inner.seek(pos)?;
                self.pos = at;
                Ok(at)
            }
            #[cfg(not(target_family = "wasm"))]
            Backend::Threaded(t) => {
                if t.cmd_tx().send(Cmd::Seek(pos)).is_err() {
                    return Err(vaco_core::Error::Io(std::io::Error::other(
                        "async: worker thread is gone",
                    )));
                }
                // Discard any chunks already in flight from before the seek
                // was seen, until the worker's own acknowledgement arrives.
                loop {
                    match t.data_rx().recv() {
                        Ok(Msg::SeekDone(Ok(at))) => {
                            self.pos = at;
                            return Ok(at);
                        }
                        Ok(Msg::SeekDone(Err(e)) | Msg::Err(e)) => return Err(e),
                        Ok(Msg::Chunk(_) | Msg::Eof) => {}
                        Err(_) => {
                            return Err(vaco_core::Error::Io(std::io::Error::other(
                                "async: worker thread is gone",
                            )));
                        }
                    }
                }
            }
        }
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.inner_size
    }

    fn seekability(&self) -> Seekability {
        if self.inner_seekable {
            Seekability::Expensive
        } else {
            Seekability::None
        }
    }

    fn peek(&mut self, len: usize) -> vaco_core::Result<&[u8]> {
        self.top_up(len)?;
        let have = self.leftover_slice();
        Ok(have.get(..len.min(have.len())).unwrap_or(have))
    }
}

/// The `async:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsyncProtocol;

impl Protocol for AsyncProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let inner = env.registry.open(&url.rest, flags, opts, env)?;
        Ok(Box::new(AsyncSource::new(inner)))
    }
}

/// The registry entry for `async:`.
pub static ASYNC_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "async",
    long_name: "Async",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    default_whitelist: &[],
    options: None,
    proto: &AsyncProtocol,
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
    use vaco_io::MemorySource;

    #[test]
    fn reads_the_same_bytes_as_the_inner_source() {
        let data: Vec<u8> = (0u8..=255).collect();
        let inner = MemorySource::forward_only(data.clone());
        let mut src = AsyncSource::new(Box::new(inner));
        let mut got = Vec::new();
        let mut chunk = [0u8; 17];
        loop {
            let n = src.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            let Some(g) = chunk.get(..n) else { break };
            got.extend_from_slice(g);
        }
        assert_eq!(got, data);
    }

    #[test]
    fn a_forward_only_inner_reports_not_seekable() {
        let inner = MemorySource::forward_only(vec![1, 2, 3]);
        let src = AsyncSource::new(Box::new(inner));
        assert_eq!(src.seekability(), Seekability::None);
    }

    #[test]
    fn seeking_a_seekable_inner_works_across_the_worker() {
        let data: Vec<u8> = (0u8..=255).collect();
        let inner = MemorySource::new(data.clone());
        let mut src = AsyncSource::new(Box::new(inner));
        assert_eq!(src.seekability(), Seekability::Expensive);

        let mut first = [0u8; 4];
        src.read_exact(&mut first).unwrap();
        assert_eq!(first, data[..4]);

        src.seek(100).unwrap();
        let mut got = [0u8; 4];
        src.read_exact(&mut got).unwrap();
        assert_eq!(got, data[100..104]);

        // And back again, exercising a second seek on the same worker.
        src.seek(0).unwrap();
        let mut got2 = [0u8; 4];
        src.read_exact(&mut got2).unwrap();
        assert_eq!(got2, data[..4]);
    }

    #[test]
    fn size_and_position_are_reported() {
        let inner = MemorySource::new(vec![0u8; 500]);
        let mut src = AsyncSource::new(Box::new(inner));
        assert_eq!(src.size(), Some(500));
        assert_eq!(src.position(), 0);
        let mut buf = [0u8; 10];
        src.read_exact(&mut buf).unwrap();
        assert_eq!(src.position(), 10);
    }

    #[test]
    fn empty_read_is_a_no_op() {
        let inner = MemorySource::forward_only(vec![1, 2, 3]);
        let mut src = AsyncSource::new(Box::new(inner));
        assert_eq!(src.read(&mut []).unwrap(), 0);
    }

    #[test]
    fn peek_does_not_move_the_position() {
        let inner = MemorySource::forward_only(vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let mut src = AsyncSource::new(Box::new(inner));
        let peeked = src.peek(4).unwrap().to_vec();
        assert_eq!(peeked, vec![1, 2, 3, 4]);
        assert_eq!(src.position(), 0);
        let mut got = [0u8; 4];
        src.read_exact(&mut got).unwrap();
        assert_eq!(got, [1, 2, 3, 4]);
    }

    #[test]
    fn eof_then_seek_revives_the_stream() {
        let data = vec![1u8, 2, 3];
        let inner = MemorySource::new(data);
        let mut src = AsyncSource::new(Box::new(inner));
        let mut buf = [0u8; 8];
        let n = src.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(src.read(&mut buf).unwrap(), 0); // EOF
        src.seek(1).unwrap();
        let n2 = src.read(&mut buf).unwrap();
        assert_eq!(n2, 2);
        assert_eq!(&buf[..2], &[2, 3]);
    }
}
