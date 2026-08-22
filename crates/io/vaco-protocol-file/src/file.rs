//! The `file:` protocol.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

use vaco_time::{Instant, sleep};

use vaco_core::{Error, Result as CoreResult};
use vaco_io::{CancelToken, MediaSink, MediaSource, PeekSource, RawSource, Seekability};
use vaco_opts::{Dict, Options, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, DirEntry, EntryKind, IoFlags, Protocol, ProtocolDesc, ProtocolError, ProtocolFlags,
    Result, Url,
};

use crate::path::{confine, url_to_path};

/// How long a `follow` read waits for the writer before reporting EOF.
const FOLLOW_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a `follow` read re-checks.
const FOLLOW_POLL: Duration = Duration::from_millis(10);

/// Options of the `file:` protocol. Names are interface facts (D9).
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "file", help = "read from or write to a local file")]
pub struct FileOptions {
    /// Truncate an existing file when opening it for writing.
    #[opt(
        name = "truncate",
        help = "truncate an existing output file",
        default = true,
        flags(param)
    )]
    pub truncate: bool,

    /// Suggested buffer size. Zero means the context's default.
    #[opt(
        name = "blocksize",
        help = "io buffer size in bytes, 0 for the default",
        default = 0,
        range = 0..=i32::MAX,
        flags(param)
    )]
    pub blocksize: i32,

    /// Keep reading a file that is still being written, instead of stopping at
    /// the current end.
    #[opt(
        name = "follow",
        help = "keep reading a file that is still being written",
        default = false,
        flags(decoding)
    )]
    pub follow: bool,
}

/// A local file as a transport.
///
/// `RawSource` rather than `MediaSource`: the peek window comes from
/// [`PeekSource`] and the read buffer from
/// [`IoContext`](vaco_io::IoContext), so this type is nothing but syscalls.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    pos: u64,
    follow: Option<FollowState>,
}

#[derive(Debug)]
struct FollowState {
    cancel: CancelToken,
    timeout: Duration,
}

impl FileSource {
    /// Wrap an already-open file.
    #[must_use]
    pub const fn new(file: File) -> Self {
        Self {
            file,
            pos: 0,
            follow: None,
        }
    }

    /// Tail the file rather than stopping at its current end.
    #[must_use]
    pub fn following(mut self, cancel: CancelToken, timeout: Option<Duration>) -> Self {
        self.follow = Some(FollowState {
            cancel,
            timeout: timeout.unwrap_or(FOLLOW_DEFAULT_TIMEOUT),
        });
        self
    }

    fn read_once(&mut self, buf: &mut [u8]) -> CoreResult<usize> {
        loop {
            return match self.file.read(buf) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(Error::from(e)),
            };
        }
    }
}

/// A filesystem timestamp as microseconds since the Unix epoch.
///
/// `SystemTime` is not orderable against the epoch without asking, so both
/// directions are tried; a pre-1970 mtime is unusual but not invalid and comes
/// back negative rather than clamped. Anything outside `i64` microseconds —
/// which is ±292,000 years and only reachable from a corrupt inode — is
/// reported as unknown rather than wrapped into a plausible-looking wrong date.
// time-gate: converting a value the OS already handed us, not reading a clock.
// `fs::Metadata::modified()` is the OS call and it already returned; from here
// on this is arithmetic. A target without a filesystem never reaches it.
fn epoch_micros(t: std::time::SystemTime) -> Option<i64> {
    // time-gate: as above — a constant, not a clock read.
    use std::time::UNIX_EPOCH;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_micros()).ok(),
        Err(e) => i64::try_from(e.duration().as_micros())
            .ok()
            .map(i64::wrapping_neg),
    }
}

impl RawSource for FileSource {
    fn read(&mut self, buf: &mut [u8]) -> CoreResult<usize> {
        let n = self.read_once(buf)?;
        if n > 0 {
            self.pos = self.pos.saturating_add(n as u64);
            return Ok(n);
        }
        // `follow`: a zero read means "not written yet", not "finished". Poll
        // rather than block, so cancellation is bounded by FOLLOW_POLL rather
        // than by the writer.
        let Some(state) = &self.follow else {
            return Ok(0);
        };
        let (cancel, timeout) = (state.cancel.clone(), state.timeout);
        // Bounded twice over, by the deadline *and* by a poll count, because
        // neither bound is sufficient alone:
        //
        // - The deadline is the real one on a target with a clock.
        // - The count is what makes this terminate on a target without one.
        //   `vaco_time::Instant` is a *stopped* clock where there is no
        //   monotonic source, so `now() < deadline` would stay true forever and
        //   this loop would never exit. Switching to `vaco-time` alone would
        //   have turned `std::time::Instant::now()`'s wasm panic into a hang,
        //   which is not an improvement — the loop shape had to change too.
        //
        // The count is derived from the same two durations, so on a working
        // clock it expires at or after the deadline and never truncates a wait.
        let deadline = Instant::now().saturating_add(timeout);
        let max_polls = timeout
            .as_nanos()
            .div_ceil(FOLLOW_POLL.as_nanos().max(1))
            .try_into()
            .unwrap_or(usize::MAX);
        for _ in 0..max_polls {
            if Instant::now() >= deadline {
                break;
            }
            cancel.check()?;
            sleep(FOLLOW_POLL);
            let n = self.read_once(buf)?;
            if n > 0 {
                self.pos = self.pos.saturating_add(n as u64);
                return Ok(n);
            }
        }
        Ok(0)
    }

    fn seek(&mut self, pos: u64) -> CoreResult<u64> {
        let at = self.file.seek(SeekFrom::Start(pos))?;
        self.pos = at;
        Ok(at)
    }

    fn size(&self) -> Option<u64> {
        // Re-stat every time: a file being written grows, and a cached length
        // would make `follow` stop early.
        self.file.metadata().ok().map(|m| m.len())
    }

    fn seekability(&self) -> Seekability {
        Seekability::Cheap
    }
}

/// A local file as an output.
#[derive(Debug)]
pub struct FileSink {
    file: File,
    pos: u64,
}

impl FileSink {
    /// Wrap an already-open file.
    #[must_use]
    pub const fn new(file: File) -> Self {
        Self { file, pos: 0 }
    }
}

impl MediaSink for FileSink {
    fn write(&mut self, buf: &[u8]) -> CoreResult<()> {
        self.file.write_all(buf)?;
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> CoreResult<u64> {
        let at = self.file.seek(SeekFrom::Start(pos))?;
        self.pos = at;
        Ok(at)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.file.flush()?;
        Ok(())
    }
}

/// The `file:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileProtocol;

impl FileProtocol {
    fn options(opts: &Dict) -> Result<FileOptions> {
        let mut parsed = FileOptions::default();
        parsed
            .apply_dict(opts)
            .map_err(|_| ProtocolError::Malformed {
                scheme: "file",
                detail: "bad option value",
            })?;
        Ok(parsed)
    }

    fn resolve(url: &Url, env: &vaco_protocol_core::ProtocolEnv<'_>) -> Result<std::path::PathBuf> {
        let raw = url_to_path(url)?;
        confine(&raw, env.root)
    }
}

impl Protocol for FileProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &vaco_protocol_core::ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let parsed = Self::options(opts)?;
        let path = Self::resolve(url, env)?;
        let file = OpenOptions::new()
            .read(true)
            .write(flags.write)
            .open(&path)?;
        let mut src = FileSource::new(file);
        if parsed.follow {
            src = src.following(env.cancel.clone(), env.rw_timeout);
        }
        Ok(Box::new(PeekSource::new(src)))
    }

    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &vaco_protocol_core::ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let parsed = Self::options(opts)?;
        let path = Self::resolve(url, env)?;
        let file = OpenOptions::new()
            .write(true)
            .read(flags.read)
            .create(true)
            .append(flags.append)
            .truncate(parsed.truncate && !flags.append)
            .open(&path)?;
        Ok(Box::new(FileSink::new(file)))
    }

    fn check(&self, url: &Url, env: &vaco_protocol_core::ProtocolEnv<'_>) -> Result<Access> {
        let path = Self::resolve(url, env)?;
        let meta = std::fs::metadata(&path)?;
        let read_only = meta.permissions().readonly();
        Ok(Access {
            read: true,
            write: !read_only,
        })
    }

    fn list_dir(
        &self,
        url: &Url,
        env: &vaco_protocol_core::ProtocolEnv<'_>,
    ) -> Result<Vec<DirEntry>> {
        let path = Self::resolve(url, env)?;
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            // `symlink_metadata`, so a listing reports links as links rather
            // than silently describing whatever they point at.
            let meta = entry.path().symlink_metadata().ok();
            let kind = meta.as_ref().map_or(EntryKind::Other, |m| {
                let t = m.file_type();
                if t.is_symlink() {
                    EntryKind::Symlink
                } else if t.is_dir() {
                    EntryKind::Directory
                } else if t.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                }
            });
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                size: meta.as_ref().map(std::fs::Metadata::len),
                modified: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(epoch_micros),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn delete(&self, url: &Url, env: &vaco_protocol_core::ProtocolEnv<'_>) -> Result<()> {
        let path = Self::resolve(url, env)?;
        if path.is_dir() {
            std::fs::remove_dir(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    fn rename(
        &self,
        from: &Url,
        to: &Url,
        env: &vaco_protocol_core::ProtocolEnv<'_>,
    ) -> Result<()> {
        let a = Self::resolve(from, env)?;
        let b = Self::resolve(to, env)?;
        std::fs::rename(a, b)?;
        Ok(())
    }
}

/// The registry entry for `file:`.
pub static FILE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "file",
    long_name: "local file",
    flags: ProtocolFlags::LOCAL,
    // `file` opens nothing nested, so it grants nothing.
    default_whitelist: &[],
    options: Some(file_schema),
    proto: &FileProtocol,
};

fn file_schema() -> &'static Schema {
    schema_of::<FileOptions>()
}
