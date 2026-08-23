//! `concat:` and `concatf:` — several URLs read as one continuous stream.
//!
//! # Grammar
//!
//! `concat:` takes the list inline, `|`-separated:
//! `concat:file1.ts|file2.ts|file3.ts`. Measured against `ffmpeg 8.1`:
//!
//! * The separator is a literal `|` with **no escaping**. A file whose name
//!   contains `\|` is not reachable through `concat:` — `concat:x\|y.ext`
//!   opens two entries, `x\` and `y.ext`, not one entry named `x|y.ext` (a
//!   real file by that exact name was created and the two-entry read still
//!   failed with "No such file or directory", proving the backslash was not
//!   treated as an escape).
//!
//! `concatf:` takes a URL naming a file that holds the same list, one entry
//! per line: `concatf:list.txt`. Measured:
//!
//! * Entries are newline-separated, not `|`-separated — a list file containing
//!   `a|b` on one line is one (failing) entry, not two.
//! * Each line is trimmed of leading/trailing whitespace before being opened
//!   (`"  a.nut  "` opens `a.nut`).
//! * There is no blank-line or `#`-comment skipping: an empty line is an
//!   attempt to open an empty path, which fails exactly the way opening `""`
//!   normally fails. This module does not special-case it, which is what
//!   reproduces that failure rather than silently accepting it.
//! * A trailing newline does not create a spurious empty final entry — `"a\nb\n"`
//!   opens two entries, not three. [`str::lines`] already has this property, so
//!   [`read_list_file`] uses it rather than a manual `split('\n')`.
//!
//! # Security
//!
//! Both variants open a sequence of URLs that did not come from the same place
//! the `concat:`/`concatf:` URL itself did — an inline list can be built by
//! whatever assembled the outer URL, and `concatf:`'s list is the contents of
//! an entire *file*, which is exactly the "a document another party wrote gets
//! to name the next URL" shape the whitelist gate exists for (see
//! `vaco-protocol-core::env`'s module docs). Every entry, and (for `concatf:`)
//! the list file itself, is opened through the *same* [`ProtocolEnv`] this
//! protocol was given — never a fresh, more permissive one — so
//! `default_whitelist` being empty here means a hostile list cannot reach a
//! scheme the caller did not already allow, exactly like `subfile:`. See the
//! crate docs for the measurement backing that default.
//!
//! # Deferred: entries are opened eagerly, not lazily
//!
//! `MediaSource` trait objects carry no lifetime, so a `ConcatSource` cannot
//! borrow the `&ProtocolEnv<'_>` a later entry's open would need — only
//! `Protocol::open` has that borrow. Opening every entry up front, inside
//! `open`, sidesteps the problem entirely at the cost of holding every entry's
//! transport open for the lifetime of the concatenation, rather than only the
//! one currently being read. For the file-and-pipe-shaped inputs this crate
//! targets that is a modest cost (a few extra file descriptors); a `concat:`
//! list large enough for it to matter would need a redesign carrying owned
//! whitelist/root state rather than a borrowed `ProtocolEnv`, which is real
//! work left for when a caller actually needs thousand-entry lists.

use vaco_io::{MediaSource, Seekability};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// Split a `concat:` URL's `rest` on literal `|`. No escaping — see the module
/// docs for the measurement.
#[must_use]
pub fn split_inline_list(rest: &str) -> Vec<&str> {
    rest.split('|').collect()
}

/// Parse a `concatf:` list file's contents into entries.
///
/// One entry per line, trimmed, with no comment or blank-line skipping. Uses
/// [`str::lines`] rather than `split('\n')` so a trailing newline does not
/// manufacture a spurious empty final entry — see the module docs.
#[must_use]
pub fn read_list_file(contents: &str) -> Vec<&str> {
    contents.lines().map(str::trim).collect()
}

/// A [`MediaSource`] over the concatenation of already-opened `sources`, read
/// in order: source `k+1` is not touched until source `k` reaches EOF.
pub struct ConcatSource {
    sources: std::collections::VecDeque<Box<dyn MediaSource>>,
    pos: u64,
}

impl std::fmt::Debug for ConcatSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcatSource")
            .field("remaining_entries", &self.sources.len())
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl ConcatSource {
    /// Wrap already-opened `sources`, read back to back in order.
    #[must_use]
    pub fn new(sources: Vec<Box<dyn MediaSource>>) -> Self {
        Self {
            sources: sources.into_iter().collect(),
            pos: 0,
        }
    }
}

impl MediaSource for ConcatSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        loop {
            let Some(cur) = self.sources.front_mut() else {
                return Ok(0);
            };
            let n = cur.read(buf)?;
            if n > 0 {
                self.pos = self.pos.saturating_add(n as u64);
                return Ok(n);
            }
            // This entry is exhausted; drop it and try the next.
            self.sources.pop_front();
        }
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        let _ = pos;
        // Seeking across entry boundaries needs each entry's size known up
        // front, which this type does not compute (most transports this
        // wraps — `pipe:`, `http:` without a `Content-Length` — cannot supply
        // it anyway). Forward-only is the honest capability to report; a
        // caller that needs seeking wraps this in `cache:`, which is exactly
        // what turns a forward-only source seekable.
        Err(vaco_core::Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn seekability(&self) -> Seekability {
        Seekability::None
    }

    fn peek(&mut self, len: usize) -> vaco_core::Result<&[u8]> {
        let Some(cur) = self.sources.front_mut() else {
            return Ok(&[]);
        };
        cur.peek(len)
    }
}

/// Open every entry in `urls`, in order, through `env`.
///
/// # Errors
/// Whatever the first failing entry reports; entries after it are never
/// attempted.
fn open_all(
    urls: &[Url],
    flags: IoFlags,
    opts: &Dict,
    env: &ProtocolEnv<'_>,
) -> Result<Vec<Box<dyn MediaSource>>> {
    urls.iter()
        .map(|u| env.registry.open_parsed(u, flags, opts, env))
        .collect()
}

/// The `concat:` protocol: an inline `|`-separated list.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConcatProtocol;

impl Protocol for ConcatProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        if url.rest.is_empty() {
            return Err(ProtocolError::Malformed {
                scheme: "concat",
                detail: "empty entry list",
            });
        }
        let urls: Vec<Url> = split_inline_list(&url.rest)
            .into_iter()
            .map(vaco_protocol_core::split_url)
            .collect();
        let sources = open_all(&urls, flags, opts, env)?;
        Ok(Box::new(ConcatSource::new(sources)))
    }
}

/// The `concatf:` protocol: the list lives in a file named by the URL.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConcatFProtocol;

impl Protocol for ConcatFProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        // The list file itself is a nested open too: it is untrusted the same
        // way an HLS playlist is, if `concatf:` is reached from one.
        let mut list_src = env.registry.open(&url.rest, IoFlags::READ, opts, env)?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = list_src.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            let Some(got) = chunk.get(..n) else { break };
            bytes.extend_from_slice(got);
        }
        drop(list_src);
        let text = String::from_utf8(bytes).map_err(|_| ProtocolError::Malformed {
            scheme: "concatf",
            detail: "list file is not valid UTF-8",
        })?;
        let urls: Vec<Url> = read_list_file(&text)
            .into_iter()
            .map(vaco_protocol_core::split_url)
            .collect();
        let sources = open_all(&urls, flags, opts, env)?;
        Ok(Box::new(ConcatSource::new(sources)))
    }
}

/// The registry entry for `concat:`.
pub static CONCAT_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "concat",
    long_name: "Virtual concatenation script",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    default_whitelist: &[],
    options: None,
    proto: &ConcatProtocol,
};

/// The registry entry for `concatf:`.
pub static CONCATF_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "concatf",
    long_name: "Virtual concatenation script (file list)",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    default_whitelist: &[],
    options: None,
    proto: &ConcatFProtocol,
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

    #[test]
    fn inline_list_splits_on_literal_pipe_with_no_escaping() {
        assert_eq!(
            split_inline_list("a.ts|b.ts|c.ts"),
            vec!["a.ts", "b.ts", "c.ts"]
        );
        // Measured: a backslash before `|` is not an escape.
        assert_eq!(split_inline_list(r"x\|y.ext"), vec![r"x\", "y.ext"]);
    }

    #[test]
    fn list_file_is_newline_not_pipe_separated() {
        assert_eq!(read_list_file("a.nut\nb.nut\n"), vec!["a.nut", "b.nut"]);
        // A trailing newline does not add a spurious empty entry.
        assert_eq!(read_list_file("a.nut\n").len(), 1);
        // But an interior blank line is kept as its own (failing) entry.
        assert_eq!(read_list_file("a.nut\n\nb.nut\n").len(), 3);
    }

    #[test]
    fn list_file_entries_are_trimmed() {
        assert_eq!(read_list_file("  a.nut  \nb.nut\n"), vec!["a.nut", "b.nut"]);
    }

    /// A minimal protocol that yields its own `rest` as bytes, for exercising
    /// `ConcatSource` without depending on `vaco-protocol-file`.
    #[derive(Debug)]
    struct Echo;
    impl Protocol for Echo {
        fn open(
            &self,
            url: &Url,
            _f: IoFlags,
            _o: &Dict,
            _e: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSource>> {
            Ok(Box::new(vaco_io::MemorySource::new(
                url.rest.clone().into_bytes(),
            )))
        }
    }
    static ECHO: ProtocolDesc = ProtocolDesc {
        name: "echo",
        long_name: "echo",
        flags: ProtocolFlags::LOCAL,
        default_whitelist: &[],
        options: None,
        proto: &Echo,
    };

    fn echo_registry() -> vaco_protocol_core::ProtocolRegistry {
        let mut r = vaco_protocol_core::ProtocolRegistry::new();
        r.register(&ECHO);
        r.register(&CONCAT_PROTOCOL);
        r
    }

    #[test]
    fn concatenated_read_crosses_entry_boundaries() {
        let registry = echo_registry();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let mut src = registry
            .open(
                "concat:echo:foo|echo:bar|echo:baz",
                IoFlags::READ,
                &Dict::new(),
                &env,
            )
            .unwrap();
        let mut all = Vec::new();
        let mut chunk = [0u8; 2];
        loop {
            let n = src.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            let Some(got) = chunk.get(..n) else { break };
            all.extend_from_slice(got);
        }
        assert_eq!(all, b"foobarbaz");
    }

    #[test]
    fn concat_is_not_seekable() {
        let mut src = ConcatSource::new(vec![Box::new(vaco_io::MemorySource::new(vec![1, 2, 3]))]);
        assert_eq!(src.seekability(), Seekability::None);
        assert!(src.seek(0).is_err());
    }

    #[test]
    fn empty_concat_list_is_malformed() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("concat:");
        assert!(matches!(
            ConcatProtocol.open(&url, IoFlags::READ, &Dict::new(), &env),
            Err(ProtocolError::Malformed {
                scheme: "concat",
                ..
            })
        ));
    }

    #[test]
    fn a_hostile_entry_scheme_is_denied_not_opened() {
        // W3-shaped: `concat:`'s own `default_whitelist` is empty, so a caller
        // that whitelists only `concat` cannot have an entry reach `echo`.
        let registry = echo_registry();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["concat"]);
        let err = registry
            .open("concat:echo:x", IoFlags::READ, &Dict::new(), &env)
            .err()
            .expect("expected the nested echo: open to be denied");
        assert!(matches!(
            err,
            vaco_protocol_core::ProtocolError::Denied {
                reason: vaco_protocol_core::DenyReason::NotWhitelisted,
                ..
            }
        ));
    }

    #[test]
    fn concatf_reads_the_list_from_a_real_file_and_crosses_entries() {
        // The list file names two `echo:` entries; `file:` supplies the list
        // itself, and `echo:` supplies each entry — a realistic stand-in for
        // "the list names ordinary media files".
        let dir = tempfile::tempdir().unwrap();
        let list_path = dir.path().join("list.txt");
        std::fs::write(&list_path, "  echo:foo  \necho:bar\n").unwrap();

        let mut registry = echo_registry();
        vaco_protocol_file::register(&mut registry);
        registry.register(&CONCATF_PROTOCOL);
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);

        let url = format!("concatf:{}", list_path.to_str().unwrap());
        let mut src = registry
            .open(&url, IoFlags::READ, &Dict::new(), &env)
            .unwrap();
        let mut all = Vec::new();
        let mut chunk = [0u8; 3];
        loop {
            let n = src.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            let Some(g) = chunk.get(..n) else { break };
            all.extend_from_slice(g);
        }
        assert_eq!(all, b"foobar");
    }
}
