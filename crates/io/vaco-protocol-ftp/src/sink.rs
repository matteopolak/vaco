//! [`FtpSink`] — `STOR` over a passive data connection.

use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_io::MediaSink;
use vaco_protocol_core::ProtocolEnv;

use crate::control::Session;

/// A `STOR` in progress.
pub struct FtpSink {
    control: Session,
    control_host: String,
    path: String,
    timeout: Option<Duration>,
    data: Option<TcpStream>,
    pos: u64,
    seekable: bool,
    finished: bool,
}

impl std::fmt::Debug for FtpSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpSink")
            .field("path", &self.path)
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl FtpSink {
    /// Run the measured `REST 0` / `SIZE` / `EPSV`-or-`PASV` / `STOR`
    /// sequence and open the data connection.
    ///
    /// `env`'s `"tcp"` grant is checked once, here, for the whole session —
    /// see `crate::source`'s module docs (the same reasoning applies here:
    /// `MediaSink::seek` has no `env` parameter).
    ///
    /// # Errors
    /// [`vaco_protocol_core::ProtocolError::Denied`] if `"tcp"` is not
    /// permitted by `env`; otherwise whatever [`Session`]'s own commands
    /// report.
    pub fn open(
        mut control: Session,
        control_host: String,
        path: String,
        timeout: Option<Duration>,
        seekable: bool,
        env: &ProtocolEnv<'_>,
    ) -> Result<Self> {
        env.check_scheme("tcp")?;
        control.rest(0)?;
        let _ = control.size(&path)?;
        let data = start_stor(&mut control, &control_host, &path, timeout)?;
        Ok(Self {
            control,
            control_host,
            path,
            timeout,
            data: Some(data),
            pos: 0,
            seekable,
            finished: false,
        })
    }

    /// Close the data connection (if still open) and read the server's
    /// final response. Idempotent, following `Md5Sink`'s pattern exactly,
    /// so an explicit `flush` and the `Drop` backstop never race.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(data) = self.data.take() {
            drop(data); // closing signals EOF to the server
        }
        let _ = self.control.finish_transfer();
        self.control.quit();
    }
}

/// See `crate::source::start_retr`'s docs for why this does not itself
/// check the whitelist.
fn start_stor(
    control: &mut Session,
    control_host: &str,
    path: &str,
    timeout: Option<Duration>,
) -> Result<TcpStream> {
    let data_addr = control.passive(control_host)?;
    let stream = vaco_protocol_socket::addr::connect(&data_addr, timeout)?;
    control.stor(path)?;
    Ok(stream)
}

impl MediaSink for FtpSink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        let Some(data) = self.data.as_mut() else {
            return Err(Error::Unsupported("ftp: write after finish"));
        };
        data.write_all(buf)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        if !self.seekable {
            return Err(Error::NotSeekable);
        }
        // Untested against a real server: resuming a STOR mid-stream with
        // REST generally requires the server to already hold the bytes up
        // to that offset (a true "seek" during upload is not otherwise a
        // meaningful FTP operation). This crate's own fake server accepts
        // any REST/STOR pair unconditionally, so the client-side command
        // sequence is exercised but real server-side resume semantics are
        // not — see the crate docs.
        if let Some(data) = self.data.take() {
            drop(data);
        }
        let _ = self.control.finish_transfer();
        self.control.rest(pos)?;
        let data = start_stor(&mut self.control, &self.control_host, &self.path, self.timeout)?;
        self.data = Some(data);
        self.pos = pos;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn flush(&mut self) -> Result<()> {
        self.finish();
        Ok(())
    }
}

impl Drop for FtpSink {
    fn drop(&mut self) {
        self.finish();
    }
}
