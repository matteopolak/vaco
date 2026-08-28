//! [`FtpSource`] — `RETR` over a passive data connection.

use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_io::{MediaSource, Seekability};
use vaco_protocol_core::ProtocolEnv;

use crate::control::Session;

/// A `RETR` in progress, plus everything needed to abort and restart it at a
/// new offset for [`MediaSource::seek`].
pub struct FtpSource {
    control: Session,
    control_host: String,
    path: String,
    timeout: Option<Duration>,
    data: Option<TcpStream>,
    pos: u64,
    size: Option<u64>,
}

impl std::fmt::Debug for FtpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpSource")
            .field("path", &self.path)
            .field("pos", &self.pos)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl FtpSource {
    /// Run the measured `REST 0` / `SIZE` / `EPSV`-or-`PASV` / `RETR`
    /// sequence and open the data connection.
    ///
    /// `env`'s `"tcp"` grant is checked once, here, for the whole session —
    /// see the module docs on why [`MediaSource::seek`]'s later data-
    /// connection reopens cannot re-check it themselves.
    ///
    /// # Errors
    /// [`vaco_core::Error`] wrapping [`vaco_protocol_core::ProtocolError::Denied`]
    /// if `"tcp"` is not permitted by `env`; otherwise whatever
    /// [`Session`]'s own commands report.
    pub fn open(
        mut control: Session,
        control_host: String,
        path: String,
        timeout: Option<Duration>,
        env: &ProtocolEnv<'_>,
    ) -> Result<Self> {
        env.check_scheme("tcp")?;
        control.rest(0)?;
        let size = control.size(&path)?;
        let data = start_retr(&mut control, &control_host, &path, timeout)?;
        Ok(Self {
            control,
            control_host,
            path,
            timeout,
            data: Some(data),
            pos: 0,
            size,
        })
    }
}

/// One `EPSV`-or-`PASV` negotiation, dial, and `RETR`.
///
/// Does **not** itself call `env.check_scheme("tcp")` — see
/// [`FtpSource::open`]'s docs and the module docs: `MediaSource::seek` has no
/// `env` parameter to check against (the trait was not designed to retain
/// one across calls), so the whitelist decision for every data connection a
/// session opens, including ones opened by a later `seek`, is made exactly
/// once, at `open()`/`create()` time. The scheme these connections use never
/// changes within a session (`"tcp"`, always), so this reuses one decision
/// rather than re-deriving it from state it does not have.
fn start_retr(
    control: &mut Session,
    control_host: &str,
    path: &str,
    timeout: Option<Duration>,
) -> Result<TcpStream> {
    let data_addr = control.passive(control_host)?;
    let stream = vaco_protocol_socket::addr::connect(&data_addr, timeout)?;
    control.retr(path)?;
    Ok(stream)
}

impl MediaSource for FtpSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let Some(data) = self.data.as_mut() else {
            return Ok(0);
        };
        let n = data.read(buf)?;
        if n == 0 {
            self.data = None;
            let _ = self.control.finish_transfer();
        } else {
            self.pos += n as u64;
        }
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        // Abort whatever transfer is in flight and drain the server's
        // response to it — RFC 959's ABOR — before starting a fresh RETR at
        // the new offset. Untested against a real server: this crate's own
        // fake FTP server (see the crate docs) always answers with a plain
        // "226 Transfer complete" regardless of ABOR, so the *shape* of this
        // sequence is exercised but a real server's exact ABOR reply
        // sequence (RFC 959 allows either one or two response lines) is
        // not.
        if self.data.take().is_some() {
            let _ = self.control.finish_transfer();
        }
        self.control.rest(pos)?;
        let data = start_retr(&mut self.control, &self.control_host, &self.path, self.timeout)?;
        self.data = Some(data);
        self.pos = pos;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn seekability(&self) -> Seekability {
        if self.size.is_some() {
            Seekability::Expensive
        } else {
            Seekability::None
        }
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        let _ = len;
        Err(Error::Unsupported("ftp: peek is not implemented"))
    }
}
