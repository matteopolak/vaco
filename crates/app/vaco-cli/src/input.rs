//! Opening an input: protocol → probe → demuxer → stream discovery.
//!
//! # The env is threaded, never rebuilt
//!
//! [`vaco_protocol_core::ProtocolEnv`] carries the `-protocol_whitelist` and
//! `-protocol_blacklist` decision, the cancellation token and the nesting
//! depth. A probed open reads the URL **twice** — once through an `IoContext`
//! that peeks and is then dropped, once for the demuxer — and both opens go
//! through the same env value. Rebuilding it for the second open is how a
//! whitelist silently stops applying to the open that actually reads the file,
//! so the env is constructed once per input and captured by the opener.
//!
//! # Two opens, and why that is a reported defect rather than a design
//!
//! `IoContext` has no `into_source`, so the probe's source cannot be handed to
//! the demuxer. This is correct for a seekable transport and **wrong for a
//! pipe**, which cannot be reopened: `vaco -i pipe:0` probes the first bytes,
//! drops the source, and reopens an already-consumed stdin. `vaco-probe` hit
//! the same wall and reported it; the fix belongs in `vaco-io`. `-f <name>`
//! skips probing and opens once, which is the working path for a pipe today.
//!
//! # Discovery is composed here
//!
//! `read_header` may only report what the header states. Matroska's start time,
//! MPEG-TS's codec parameters and everyone's frame rate need packets, and
//! [`Discovery`] is that pass: it reads a bounded prefix, refines what it can,
//! and replays everything it consumed. It is a wrapper rather than a driver, so
//! only a caller that owns the demuxer can compose it — which is here.

use vaco_core::{Error, Result};
use vaco_format_core::{Demuxer, DemuxerDesc, Discovery, FormatOptions, Probe};
use vaco_io::{CancelToken, IoContext, IoOptions, MediaSource};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolError, ProtocolRegistry};

/// One opened input file.
pub struct InputFile {
    /// Position among input files: the number `-map 0:…` names.
    pub index: u32,
    /// The URL as written.
    pub url: String,
    pub demuxer: Box<dyn Demuxer>,
    pub desc: DemuxerDesc,
    /// The transport's own byte length, when it can report one — a pipe
    /// cannot. #641's `Input #0` dump computes `bitrate:` from this and the
    /// container's duration, the same way `vaco-probe`'s `-show_format` does
    /// for the same field. Only known on the probed-open path: `-f <name>`
    /// skips the peek this is read from, so a forced format reports `None`
    /// here even for a perfectly ordinary seekable file.
    pub size: Option<u64>,
}

impl core::fmt::Debug for InputFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InputFile")
            .field("index", &self.index)
            .field("url", &self.url)
            .field("format", &self.desc.name)
            .field("streams", &self.demuxer.streams().len())
            .field("size", &self.size)
            .finish()
    }
}

/// How an input is to be opened.
#[derive(Debug, Clone, Default)]
pub struct OpenRequest<'a> {
    /// `-f <name>`: skip probing and force this demuxer.
    pub force_format: Option<&'a str>,
    /// `-protocol_whitelist`.
    pub whitelist: Option<&'a [&'a str]>,
    /// `-protocol_blacklist`.
    pub blacklist: Option<&'a [&'a str]>,
    /// FW-11: `-probesize`, `-analyzeduration`, `-fflags` and the rest of the
    /// generic `AVFormatContext` options this input group named
    /// ([`crate::cli::InputSpec::format_opts`]). `None` (every pre-existing
    /// caller, including this module's own tests) means
    /// [`FormatOptions::default`]. Fed to [`Probe`] and [`Discovery`] below —
    /// `Discovery::run` is what actually reaches
    /// [`vaco_format_core::Demuxer::reconfigure`] with it (gap 4).
    pub format_opts: Option<&'a FormatOptions>,
}

/// Open, probe and wrap one input.
///
/// # Errors
///
/// Whatever the protocol, the probe or the demuxer reported, unwrapped so the
/// caller can read the `io::ErrorKind` — `ENOENT`, `EACCES` and `EISDIR` are
/// three different exit codes and collapsing them changes observable output.
pub fn open(index: u32, url: &str, req: &OpenRequest<'_>) -> Result<InputFile> {
    // `file:` and `pipe:` ship no `vaco-component.toml`, so the generated
    // registry has no protocols at all. Registering the file protocol
    // explicitly is the same gap `vaco-probe` reported; it belongs in those
    // crates as a fragment, not here.
    let mut protocols = vaco_registry::protocol_registry();
    vaco_protocol_file::register(&mut protocols);

    let cancel = CancelToken::new();
    let env = build_env(&protocols, &cancel, req);
    let opener = |u: &str| -> Result<Box<dyn MediaSource>> { open_source(&protocols, &env, u) };

    let owned_default;
    let format_opts: &FormatOptions = if let Some(o) = req.format_opts {
        o
    } else {
        owned_default = FormatOptions::default();
        &owned_default
    };
    let probe = Probe::new(vaco_registry::demuxers(), format_opts);

    let mut size = None;
    let desc: DemuxerDesc = if let Some(name) = req.force_format {
        *probe.force(name)?.desc
    } else {
        let mut io = IoContext::new(opener(url)?, &IoOptions::default())?;
        size = io.size();
        *probe.detect(&mut io, Some(url), None)?.desc
    };

    let inner = if desc.flags.contains(vaco_format_core::FormatFlags::NEEDNUMBER) {
        // Gap 7/#649: `img_%03d.png` is a pattern, not an openable file, so
        // `opener(url)` would fail before the demuxer ever got a say — the
        // exact bug reported against `-f image2` input. `DemuxerDesc::open`'s
        // frozen signature still needs *some* source, so hand it an empty
        // placeholder and let `Demuxer::bind_url` do the real filesystem
        // resolution `Image2Demuxer::open_pattern` already implements
        // correctly once it has the real URL.
        let placeholder: Box<dyn MediaSource> = Box::new(vaco_io::MemorySource::new(Vec::new()));
        let mut inner = (desc.open)(placeholder, &vaco_registry::Parsers)?;
        inner.bind_url(url)?;
        inner
    } else {
        let mut inner = (desc.open)(opener(url)?, &vaco_registry::Parsers)?;
        // Best-effort for a format whose primary source opened fine but that
        // still wants its own URL for something extra (VobSub's `.sub`
        // sidecar, gap 7's other named case): `Unsupported` just means this
        // demuxer has nothing to bind, which is every demuxer today.
        match inner.bind_url(url) {
            Ok(()) | Err(Error::Unsupported(_)) => {}
            Err(e) => return Err(e),
        }
        inner
    };

    let mut discovery = Discovery::new(inner, desc.flags, format_opts);
    // A failed discovery pass is not a failed open: it keeps whatever it
    // learned, and `read_header` already produced a usable stream list.
    let _ = discovery.run(&vaco_registry::Parsers);

    Ok(InputFile {
        index,
        url: url.to_owned(),
        demuxer: Box::new(discovery),
        desc,
        size,
    })
}

fn build_env<'a>(
    protocols: &'a ProtocolRegistry,
    cancel: &'a CancelToken,
    req: &OpenRequest<'a>,
) -> ProtocolEnv<'a> {
    let mut env = ProtocolEnv::new(protocols, cancel);
    if let Some(w) = req.whitelist {
        env = env.with_whitelist(w);
    }
    if let Some(b) = req.blacklist {
        env = env.with_blacklist(b);
    }
    env
}

fn open_source(
    protocols: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    url: &str,
) -> Result<Box<dyn MediaSource>> {
    protocols
        .open(url, IoFlags::READ, &Dict::new(), env)
        .map_err(|e| match e {
            // Unwrapped rather than re-wrapped: the exit code is derived from
            // the `io::ErrorKind`, and `Error::Option` would lose it.
            ProtocolError::Io(inner) => inner,
            other => Error::Option {
                name: "i".to_owned(),
                detail: other.to_string(),
            },
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_reports_not_found_not_a_generic_failure() {
        let e = open(0, "/nonexistent/vaco-cli-test.mkv", &OpenRequest::default()).unwrap_err();
        match e {
            Error::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_whitelist_that_excludes_file_blocks_a_local_path() {
        let list: &[&str] = &["http"];
        let req = OpenRequest {
            whitelist: Some(list),
            ..OpenRequest::default()
        };
        let e = open(0, "/etc/hosts", &req).unwrap_err();
        // Not `NotFound`: the file exists, and the whitelist is what stopped it.
        assert!(
            !matches!(&e, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound),
            "{e:?}"
        );
    }

    #[test]
    fn a_directory_reports_is_a_directory() {
        // The reference exits 235 here, which is `EISDIR` truncated. The kind
        // has to survive the trip for that to be reproducible.
        let e = open(0, "/tmp", &OpenRequest::default()).unwrap_err();
        match e {
            Error::Io(io) => assert!(
                matches!(
                    io.kind(),
                    std::io::ErrorKind::IsADirectory | std::io::ErrorKind::PermissionDenied
                ),
                "{io:?}"
            ),
            // Some platforms surface a directory read as invalid data instead;
            // either way it must not be a success.
            other => assert!(matches!(other, Error::InvalidData(_)), "{other:?}"),
        }
    }

    /// Gap 7/#649: `-f image2` on a `%03d` pattern used to fail before the
    /// demuxer ever saw the URL, because `opener(url)` tried to open the
    /// literal, non-existent pattern string. `DEMUXER_IMAGE2.flags` carries
    /// `NEEDNUMBER`, which routes this open through the placeholder-source +
    /// `bind_url` path instead.
    #[test]
    fn an_image2_pattern_forced_by_format_opens_every_matching_file() {
        let dir = std::env::temp_dir().join(format!(
            "vaco-cli-input-test-image2-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("img001.png"), b"one").unwrap();
        std::fs::write(dir.join("img002.png"), b"two").unwrap();
        let pattern = dir.join("img%03d.png");
        let pattern = pattern.to_str().unwrap();

        let req = OpenRequest {
            force_format: Some("image2"),
            ..OpenRequest::default()
        };
        let mut file = open(0, pattern, &req).unwrap();
        assert_eq!(file.demuxer.streams().len(), 1);
        assert_eq!(file.demuxer.read_packet().unwrap().payload(), b"one");
        assert_eq!(file.demuxer.read_packet().unwrap().payload(), b"two");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
