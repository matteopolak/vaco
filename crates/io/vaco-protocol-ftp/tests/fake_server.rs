//! End-to-end against an in-process fake FTP control+data server —
//! reproduces (in Rust, so no external interpreter is needed to run the
//! suite) the same fake server used to capture the command sequence in the
//! crate docs, driving the real `Protocol::open`/`create` path rather than
//! `control::Session` in isolation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use vaco_io::{CancelToken, MediaSource};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_ftp::FTP_PROTOCOL);
    r
}

/// What the fake server should do with a data connection once one is
/// negotiated.
enum DataBehavior {
    /// Send this content, then close.
    Send(Vec<u8>),
    /// Receive until close, then hand the bytes back over `report`.
    Receive(mpsc::Sender<Vec<u8>>),
}

/// A minimal FTP control+data server. Answers the exact command sequence
/// this crate's `control::Session` sends (see the crate docs), including an
/// `EPSV`-then-`PASV` fallback path when `epsv_supported` is `false`.
struct FakeFtp {
    control_addr: std::net::SocketAddr,
}

impl FakeFtp {
    fn start(epsv_supported: bool, data: DataBehavior) -> Self {
        let control = TcpListener::bind("127.0.0.1:0").unwrap();
        let data_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let control_addr = control.local_addr().unwrap();
        let data_port = data_listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (conn, _) = control.accept().unwrap();
            run_control(conn, data_port, epsv_supported);
        });
        thread::spawn(move || {
            let (mut conn, _) = data_listener.accept().unwrap();
            match data {
                DataBehavior::Send(bytes) => {
                    conn.write_all(&bytes).unwrap();
                }
                DataBehavior::Receive(report) => {
                    let mut buf = Vec::new();
                    conn.read_to_end(&mut buf).unwrap();
                    let _ = report.send(buf);
                }
            }
        });

        Self { control_addr }
    }
}

fn send(conn: &mut TcpStream, line: &str) {
    conn.write_all(line.as_bytes()).unwrap();
}

fn recv_line(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line.trim_end().to_owned()
}

fn run_control(conn: TcpStream, data_port: u16, epsv_supported: bool) {
    let mut writer = conn.try_clone().unwrap();
    let mut reader = BufReader::new(conn);
    send(&mut writer, "220 Fake FTP ready\r\n");
    loop {
        let line = recv_line(&mut reader);
        if line.is_empty() {
            break;
        }
        let cmd = line.split(' ').next().unwrap_or("").to_ascii_uppercase();
        match cmd.as_str() {
            "USER" => send(&mut writer, "331 Password required\r\n"),
            "PASS" => send(&mut writer, "230 Logged in\r\n"),
            "TYPE" => send(&mut writer, "200 Type set\r\n"),
            "FEAT" => send(&mut writer, "211 no features\r\n"),
            "PWD" => send(&mut writer, "257 \"/\" is current directory\r\n"),
            "REST" => send(&mut writer, "350 Restart position accepted\r\n"),
            "SIZE" => send(&mut writer, "213 15\r\n"),
            "EPSV" => {
                if epsv_supported {
                    send(&mut writer, &format!("229 Entering Extended Passive Mode (|||{data_port}|)\r\n"));
                } else {
                    send(&mut writer, "500 Unknown command EPSV\r\n");
                }
            }
            "PASV" => {
                if epsv_supported {
                    // A server that supports EPSV should never see a PASV
                    // from this client — proves EPSV is genuinely tried
                    // first rather than PASV always being sent regardless.
                    send(&mut writer, "500 PASV should not have been sent\r\n");
                } else {
                    let (p1, p2) = (data_port >> 8, data_port & 0xff);
                    send(&mut writer, &format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n"));
                }
            }
            "RETR" | "STOR" => {
                send(&mut writer, "150 Opening data connection\r\n");
                // The transfer happens on the data connection, out of band;
                // give it a moment, then report completion.
                thread::sleep(std::time::Duration::from_millis(100));
                send(&mut writer, "226 Transfer complete\r\n");
            }
            "QUIT" => {
                send(&mut writer, "221 Bye\r\n");
                break;
            }
            _ => send(&mut writer, "500 Unknown command\r\n"),
        }
    }
}

fn env<'a>(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> ProtocolEnv<'a> {
    ProtocolEnv::new(registry, cancel).with_whitelist(&["ftp", "tcp"])
}

#[test]
fn reads_a_file_via_epsv() {
    let content = b"Hello FTP World!".to_vec();
    let server = FakeFtp::start(true, DataBehavior::Send(content.clone()));
    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("ftp://127.0.0.1:{}/pub/file.bin", server.control_addr.port());
    let mut source = registry.open(&url, IoFlags::READ, &Dict::new(), &e).unwrap();

    let mut got = Vec::new();
    let mut buf = [0u8; 4];
    loop {
        let n = source.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, content);
}

#[test]
fn falls_back_to_pasv_when_epsv_is_refused() {
    let content = b"via pasv fallback".to_vec();
    let server = FakeFtp::start(false, DataBehavior::Send(content.clone()));
    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("ftp://127.0.0.1:{}/f.bin", server.control_addr.port());
    let mut source = registry.open(&url, IoFlags::READ, &Dict::new(), &e).unwrap();

    let mut got = Vec::new();
    source.read_exact_to_end(&mut got);
    assert_eq!(got, content);
}

/// `MediaSource::read` returns short reads; this is the small helper every
/// test in this file needs, kept local rather than added to `vaco-io`
/// (which this crate does not own).
trait ReadToEnd {
    fn read_exact_to_end(&mut self, out: &mut Vec<u8>);
}

impl ReadToEnd for Box<dyn MediaSource> {
    fn read_exact_to_end(&mut self, out: &mut Vec<u8>) {
        let mut buf = [0u8; 8];
        loop {
            let n = self.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
}

#[test]
fn writes_a_file_via_stor() {
    let (tx, rx) = mpsc::channel();
    let server = FakeFtp::start(true, DataBehavior::Receive(tx));
    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("ftp://127.0.0.1:{}/out.bin", server.control_addr.port());

    {
        let mut sink = registry.create(&url, IoFlags::WRITE, &Dict::new(), &e).unwrap();
        sink.write(b"hello ftp world").unwrap();
        sink.flush().unwrap();
    }

    let received = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(received, b"hello ftp world");
}

#[test]
fn a_whitelist_naming_only_ftp_refuses_the_nested_tcp_open() {
    let server = FakeFtp::start(true, DataBehavior::Send(vec![]));
    let registry = registry();
    let cancel = CancelToken::new();
    let e = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["ftp"]);
    let url = format!("ftp://127.0.0.1:{}/f", server.control_addr.port());
    let err = registry
        .open(&url, IoFlags::READ, &Dict::new(), &e)
        .err()
        .unwrap();
    assert!(matches!(err, vaco_protocol_core::ProtocolError::Denied { .. }));
}
