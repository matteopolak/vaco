//! The buffered reader every demuxer is handed.

use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

use crate::{CancelToken, Checksum, ChecksumKind, MediaSource, Seekability, replay};

/// Default read buffer. The same 32 KiB the reference tool uses, which matters
/// because buffer size determines how many range requests an HTTP walk makes and
/// therefore shows up in differential traces.
pub const DEFAULT_BLOCK_SIZE: usize = 32 * 1024;

/// Smallest and largest buffer we will honour, whatever an option says.
const MIN_BLOCK_SIZE: usize = 64;
const MAX_BLOCK_SIZE: usize = 16 * 1024 * 1024;

/// How much read-and-discard is cheaper than a seek on an expensive transport.
const DEFAULT_SHORT_SEEK_MAX: u64 = 64 * 1024;

/// Tunables for [`IoContext`] and [`IoWriter`](crate::IoWriter).
#[derive(Debug, Clone)]
pub struct IoOptions {
    /// Read/write buffer size. Clamped to `[64, 16 MiB]`.
    pub block_size: usize,
    /// `-avioflags direct`: bypass the buffer for reads at least as large as it,
    /// and flush after every write.
    pub direct: bool,
    /// Forward seek distance, on a transport whose seeks cost a round trip, that
    /// is cheaper as a read-and-discard.
    ///
    /// Ignored when [`MediaSource::seekability`] is [`Seekability::Cheap`] (a
    /// local seek is always cheaper) and when it is [`Seekability::None`] (a
    /// forward seek has no other implementation).
    pub short_seek_max: u64,
    /// Caps every buffer this context allocates.
    pub limits: Limits,
}

impl Default for IoOptions {
    fn default() -> Self {
        Self {
            block_size: DEFAULT_BLOCK_SIZE,
            direct: false,
            short_seek_max: DEFAULT_SHORT_SEEK_MAX,
            limits: Limits::strict(),
        }
    }
}

impl IoOptions {
    /// Override the buffer size.
    #[must_use]
    pub const fn with_block_size(mut self, n: usize) -> Self {
        self.block_size = n;
        self
    }

    /// Override the allocation policy.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Turn `-avioflags direct` on or off.
    #[must_use]
    pub const fn with_direct(mut self, direct: bool) -> Self {
        self.direct = direct;
        self
    }
}

/// The buffered, byte-order-aware reader demuxers use.
///
/// # What it adds over the source
///
/// * a single read buffer, so `r8()` in a loop is not a syscall in a loop;
/// * backward seeks inside the buffer, which every box and element parser needs
///   because they all overshoot and rewind;
/// * forward short seeks as read-and-discard, which is what stops an MP4 `moov`
///   walk from opening one HTTP connection per box;
/// * a sticky error and a sticky EOF, so a demuxer unwinds cleanly instead of
///   retrying a dead transport at every level;
/// * running checksums over consumed bytes.
///
/// # Position model
///
/// `base` is the logical offset of `buf[0]`; the caller's position is
/// `base + head`. Every method preserves `head <= tail <= buf.len()`.
pub struct IoContext {
    src: Box<dyn MediaSource>,
    buf: Vec<u8>,
    head: usize,
    tail: usize,
    base: u64,
    eof: bool,
    err: Option<Error>,
    bytes_read: u64,
    checksum: Option<Checksum>,
    budget: Budget,
    cancel: CancelToken,
    seekability: Seekability,
    short_seek: u64,
    direct: bool,
    max_peek: usize,
}

impl std::fmt::Debug for IoContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoContext")
            .field("pos", &self.pos())
            .field("buffered", &(self.tail - self.head))
            .field("eof", &self.eof)
            .field("error", &self.err)
            .field("seekability", &self.seekability)
            .finish_non_exhaustive()
    }
}

impl IoContext {
    /// Wrap `src` with a buffer sized by `opts`.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if `opts.limits` will not permit the buffer.
    pub fn new(src: Box<dyn MediaSource>, opts: &IoOptions) -> Result<Self> {
        let block = opts.block_size.clamp(MIN_BLOCK_SIZE, MAX_BLOCK_SIZE);
        let mut budget = Budget::new(opts.limits.clone());
        let buf = budget.alloc::<u8>(block)?;
        let seekability = src.seekability();
        let base = src.position();
        let short_seek = match seekability {
            // A local seek is always cheaper than reading bytes we throw away.
            Seekability::Cheap => 0,
            Seekability::Expensive => opts.short_seek_max,
            // Forward-only: read-and-discard is the *only* implementation.
            Seekability::None => u64::MAX,
        };
        let max_peek = usize::try_from(opts.limits.max_probe_bytes).unwrap_or(usize::MAX);
        Ok(Self {
            src,
            buf,
            head: 0,
            tail: 0,
            base,
            eof: false,
            err: None,
            bytes_read: 0,
            checksum: None,
            budget,
            cancel: CancelToken::new(),
            seekability,
            short_seek,
            direct: opts.direct,
            max_peek,
        })
    }

    /// Attach a cancellation token, checked before every transport read.
    pub fn set_cancel(&mut self, cancel: CancelToken) {
        self.cancel = cancel;
    }

    // ------------------------------------------------------------------ shape

    /// The caller's byte offset.
    #[must_use]
    pub const fn pos(&self) -> u64 {
        self.base.saturating_add(self.head as u64)
    }

    /// Unconsumed bytes already in the buffer.
    #[must_use]
    pub const fn buffered(&self) -> usize {
        self.tail.saturating_sub(self.head)
    }

    /// Total size, when the transport knows it.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        self.src.size()
    }

    /// What the transport can do.
    #[must_use]
    pub const fn seekability(&self) -> Seekability {
        self.seekability
    }

    /// Whether the source is exhausted *and* the buffer is drained.
    #[must_use]
    pub const fn at_eof(&self) -> bool {
        self.eof && self.buffered() == 0
    }

    /// Bytes pulled from the transport, for `probesize` accounting. Not the same
    /// as [`IoContext::pos`]: a backward seek rewinds the position, never this.
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// The sticky error, if the context has failed.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        self.err.as_ref()
    }

    /// Clear the sticky error so reads are attempted again.
    ///
    /// Only correct after the caller has done something to make progress
    /// possible — a seek to a known-good offset, say.
    pub fn clear_error(&mut self) {
        self.err = None;
    }

    fn fail(&mut self, e: Error) -> Error {
        let out = replay(&e);
        if self.err.is_none() {
            self.err = Some(e);
        }
        out
    }

    // ------------------------------------------------------------- buffer fill

    /// Pull one transport read into the buffer. Sets `eof` on a zero read.
    fn fill(&mut self) -> Result<()> {
        if let Some(e) = &self.err {
            return Err(replay(e));
        }
        if self.eof {
            return Ok(());
        }
        if let Err(e) = self.cancel.check() {
            return Err(self.fail(e));
        }
        if self.head == self.tail {
            self.base = self.base.saturating_add(self.head as u64);
            self.head = 0;
            self.tail = 0;
        } else if self.tail == self.buf.len() {
            self.compact();
        }
        let Some(dst) = self.buf.get_mut(self.tail..) else {
            return Ok(());
        };
        if dst.is_empty() {
            return Ok(());
        }
        match self.src.read(dst) {
            Ok(0) => self.eof = true,
            Ok(n) => {
                self.tail += n;
                self.bytes_read = self.bytes_read.saturating_add(n as u64);
            }
            Err(e) => return Err(self.fail(e)),
        }
        Ok(())
    }

    /// Move unconsumed bytes to the front of the buffer.
    fn compact(&mut self) {
        if self.head == 0 {
            return;
        }
        self.buf.copy_within(self.head..self.tail, 0);
        self.base = self.base.saturating_add(self.head as u64);
        self.tail -= self.head;
        self.head = 0;
    }

    /// Consume `n` buffered bytes, feeding any open checksum.
    fn advance(&mut self, n: usize) {
        if let Some(c) = self.checksum.as_mut()
            && let Some(region) = self.buf.get(self.head..self.head + n)
        {
            c.update(region);
        }
        self.head += n;
    }

    // ------------------------------------------------------------------ reads

    /// Read up to `dst.len()` bytes. A short read is normal; `Ok(0)` is EOF.
    ///
    /// # Errors
    /// Propagates transport failure, and replays a sticky one.
    pub fn read_partial(&mut self, dst: &mut [u8]) -> Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        if self.buffered() == 0 {
            // `-avioflags direct`: a read at least as large as the buffer has
            // nothing to gain from going through it.
            if self.direct && dst.len() >= self.buf.len() && !self.eof {
                if let Err(e) = self.cancel.check() {
                    return Err(self.fail(e));
                }
                let n = match self.src.read(dst) {
                    Ok(n) => n,
                    Err(e) => return Err(self.fail(e)),
                };
                if n == 0 {
                    self.eof = true;
                } else {
                    self.bytes_read = self.bytes_read.saturating_add(n as u64);
                    if let Some(c) = self.checksum.as_mut()
                        && let Some(region) = dst.get(..n)
                    {
                        c.update(region);
                    }
                    self.base = self.base.saturating_add(self.head as u64 + n as u64);
                    self.head = 0;
                    self.tail = 0;
                }
                return Ok(n);
            }
            self.fill()?;
            if self.buffered() == 0 {
                return Ok(0);
            }
        }
        let n = self.buffered().min(dst.len());
        let src = self
            .buf
            .get(self.head..self.head + n)
            .ok_or(Error::UnexpectedEof)?;
        let out = dst.get_mut(..n).ok_or(Error::UnexpectedEof)?;
        out.copy_from_slice(src);
        self.advance(n);
        Ok(n)
    }

    /// Fill `dst` completely.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if the source ends first.
    pub fn read_exact(&mut self, dst: &mut [u8]) -> Result<()> {
        let mut done = 0;
        while done < dst.len() {
            let Some(rest) = dst.get_mut(done..) else {
                return Err(Error::UnexpectedEof);
            };
            match self.read_partial(rest)? {
                0 => return Err(Error::UnexpectedEof),
                n => done += n,
            }
        }
        Ok(())
    }

    /// Look at the next `len` bytes without consuming them.
    ///
    /// Returns fewer than `len` bytes only at EOF. Works on a forward-only
    /// source: the bytes are held in this context's buffer, never re-read.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if `len` exceeds `limits.max_probe_bytes`;
    /// otherwise propagates transport failure.
    pub fn peek(&mut self, len: usize) -> Result<&[u8]> {
        if len > self.max_peek {
            return Err(Error::LimitExceeded {
                limit: "max_probe_bytes",
                requested: len as u64,
                cap: self.max_peek as u64,
            });
        }
        if self.buffered() < len {
            self.compact();
            if len > self.buf.len() {
                let extra = len.saturating_sub(self.buf.len());
                self.budget.charge(extra as u64)?;
                self.buf.resize(len, 0);
            }
            while self.buffered() < len && !self.eof {
                let before = self.tail;
                self.fill()?;
                if self.tail == before {
                    break;
                }
            }
        }
        let end = (self.head + len).min(self.tail);
        self.buf.get(self.head..end).ok_or(Error::UnexpectedEof)
    }

    /// Discard `n` bytes by reading them.
    fn discard(&mut self, mut n: u64) -> Result<()> {
        while n > 0 {
            if self.buffered() == 0 {
                self.fill()?;
                if self.buffered() == 0 {
                    return Err(Error::UnexpectedEof);
                }
            }
            let k = (self.buffered() as u64).min(n) as usize;
            self.advance(k);
            n -= k as u64;
        }
        Ok(())
    }

    // -------------------------------------------------------------- byte order

    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn r8(&mut self) -> Result<u8> {
        if self.buffered() == 0 {
            self.fill()?;
            if self.buffered() == 0 {
                return Err(Error::UnexpectedEof);
            }
        }
        let b = *self.buf.get(self.head).ok_or(Error::UnexpectedEof)?;
        self.advance(1);
        Ok(b)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.read_exact(&mut b)?;
        Ok(b)
    }

    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rb16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.fixed::<2>()?))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rl16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.fixed::<2>()?))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rb24(&mut self) -> Result<u32> {
        let b = self.fixed::<3>()?;
        Ok(u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rl24(&mut self) -> Result<u32> {
        let b = self.fixed::<3>()?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], 0]))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rb32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.fixed::<4>()?))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rl32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed::<4>()?))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rb64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.fixed::<8>()?))
    }
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn rl64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.fixed::<8>()?))
    }

    /// The four-character code every RIFF-derived and ISO-BMFF container uses.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] at end of input.
    pub fn tag(&mut self) -> Result<[u8; 4]> {
        self.fixed::<4>()
    }

    // ------------------------------------------------------------------ strings

    /// Read a NUL-terminated string, stopping after `max` bytes.
    ///
    /// The NUL is consumed when present. Invalid UTF-8 is replaced rather than
    /// rejected, because container string fields are frequently mislabelled and
    /// the reference tool does not drop the field over it.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if `max` exceeds the metadata budget; propagates
    /// transport failure.
    pub fn get_str(&mut self, max: usize) -> Result<String> {
        self.budget.check(max as u64)?;
        let mut out: Vec<u8> = Vec::new();
        while out.len() < max {
            match self.r8() {
                // A missing terminator at end of input is not an error: a
                // truncated string field is the last thing in the file.
                Ok(0) | Err(Error::UnexpectedEof) => break,
                Ok(b) => out.push(b),
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Read `len` **bytes** as big-endian UTF-16. ASF and MOV `keys` use this.
    ///
    /// A trailing NUL code unit is dropped. An odd `len` drops the stray byte.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if `len` exceeds the budget; propagates
    /// transport failure.
    pub fn get_str16be(&mut self, len: usize) -> Result<String> {
        self.get_str16(len, true)
    }

    /// Little-endian counterpart of [`IoContext::get_str16be`].
    ///
    /// # Errors
    /// As [`IoContext::get_str16be`].
    pub fn get_str16le(&mut self, len: usize) -> Result<String> {
        self.get_str16(len, false)
    }

    fn get_str16(&mut self, len: usize, big_endian: bool) -> Result<String> {
        let mut units: Vec<u16> = self.budget.alloc::<u16>(len >> 1)?;
        for slot in &mut units {
            let mut b = [0u8; 2];
            self.read_exact(&mut b)?;
            *slot = if big_endian {
                u16::from_be_bytes(b)
            } else {
                u16::from_le_bytes(b)
            };
        }
        if len & 1 == 1 {
            let _ = self.r8();
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        Ok(String::from_utf16_lossy(&units))
    }

    // ------------------------------------------------------------------- seeks

    /// Move to absolute offset `pos`.
    ///
    /// In order: inside the buffer (free, including backwards); a forward hop of
    /// at most the short-seek threshold (read-and-discard); a transport seek.
    /// On a forward-only source a backward seek outside the buffer is
    /// [`Error::NotSeekable`] and a forward one is always read-and-discard.
    ///
    /// # Errors
    /// [`Error::NotSeekable`], or transport failure.
    pub fn seek(&mut self, pos: u64) -> Result<u64> {
        let win_start = self.base;
        let win_end = self.base.saturating_add(self.tail as u64);
        if pos >= win_start && pos <= win_end {
            self.head = (pos - win_start) as usize;
            return Ok(pos);
        }
        let cur = self.pos();
        if pos > cur {
            let ahead = pos - cur;
            if ahead <= self.short_seek {
                self.discard(ahead)?;
                return Ok(pos);
            }
        }
        if self.seekability == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let actual = match self.src.seek(pos) {
            Ok(p) => p,
            Err(e) => return Err(self.fail(e)),
        };
        self.base = actual;
        self.head = 0;
        self.tail = 0;
        self.eof = false;
        Ok(actual)
    }

    /// Seek to `back` bytes before the end.
    ///
    /// # Errors
    /// [`Error::NotSeekable`] when the size is unknown, otherwise as
    /// [`IoContext::seek`].
    pub fn seek_from_end(&mut self, back: u64) -> Result<u64> {
        let size = self.size().ok_or(Error::NotSeekable)?;
        self.seek(size.saturating_sub(back))
    }

    /// Skip `n` bytes forward.
    ///
    /// # Errors
    /// As [`IoContext::seek`].
    pub fn skip(&mut self, n: u64) -> Result<()> {
        let target = self.pos().saturating_add(n);
        self.seek(target).map(|_| ())
    }

    // --------------------------------------------------------------- integrity

    /// Open a checksum region at the current position.
    ///
    /// Only bytes **consumed sequentially** are fed. A seek does not feed the
    /// bytes it skips over, so a caller that seeks inside a region gets the
    /// checksum of what it actually read — which is what the container formats
    /// that use this want, and is why the region is not defined by byte range.
    pub fn start_checksum(&mut self, kind: ChecksumKind) {
        self.checksum = Some(Checksum::new(kind, self.pos()));
    }

    /// The value so far without closing the region.
    #[must_use]
    pub fn checksum(&self) -> Option<u64> {
        self.checksum.map(|c| c.value())
    }

    /// Close the region and return its value. `0` when none was open.
    pub fn take_checksum(&mut self) -> u64 {
        self.checksum.take().map_or(0, |c| c.value())
    }
}
