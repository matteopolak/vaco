//! End-to-end: a fake RTSP server on a loopback `TcpListener`, speaking
//! just enough of the protocol to get `RtspDemuxer::open` through
//! `DESCRIBE`/`SETUP`/`PLAY`, then a real UDP datagram carrying one PCMU
//! RTP packet, read back out through [`vaco_format_rtp::depacket::raw::Identity`].
//!
//! No real network is used — every socket here is `127.0.0.1`, per the
//! brief's "start a loopback server inside the test" rule.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::Duration;

use vaco_format_core::Demuxer;
use vaco_io::CancelToken;
use vaco_protocol_core::{ProtocolEnv, ProtocolRegistry};

fn read_request(reader: &mut BufReader<TcpStream>) -> (String, Vec<String>) {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end().to_owned();
        if line.is_empty() {
            break;
        }
        headers.push(line);
    }
    (request_line.trim_end().to_owned(), headers)
}

fn cseq_of(headers: &[String]) -> String {
    headers
        .iter()
        .find_map(|h| h.strip_prefix("CSeq:").map(|v| v.trim().to_owned()))
        .unwrap()
}

#[test]
fn rtsp_demuxer_open_negotiates_udp_and_reads_one_packet() {
    let rtsp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let rtsp_addr = rtsp_listener.local_addr().unwrap();

    // The RTP source the "server" sends from — its port becomes this
    // session's `server_port` in the SETUP response.
    let server_rtp_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let server_rtp_port = server_rtp_socket.local_addr().unwrap().port();

    let sdp = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
         m=audio 0 RTP/AVP 0\r\na=control:track1\r\n"
        .to_owned();

    let server = thread::spawn(move || {
        let (stream, _) = rtsp_listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        // DESCRIBE
        let (line, headers) = read_request(&mut reader);
        assert!(line.starts_with("DESCRIBE"));
        let cseq = cseq_of(&headers);
        write!(
            writer,
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
            sdp.len()
        )
        .unwrap();

        // SETUP
        let (line, headers) = read_request(&mut reader);
        assert!(line.starts_with("SETUP"));
        let cseq = cseq_of(&headers);
        write!(
            writer,
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: TESTSESSION;timeout=60\r\n\
             Transport: RTP/AVP;unicast;client_port=0-0;server_port={server_rtp_port}-{}\r\n\r\n",
            server_rtp_port + 1
        )
        .unwrap();

        // PLAY
        let (line, headers) = read_request(&mut reader);
        assert!(line.starts_with("PLAY"));
        let cseq = cseq_of(&headers);
        write!(writer, "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n\r\n").unwrap();

        // Now that PLAY has been answered, send one PCMU RTP packet to
        // whatever local port the client bound for receiving (client_port
        // was offered as 0-0, a placeholder — the client's *real* local
        // port is discovered by having the client's SETUP go out over the
        // loopback stack; since this fake server does not parse the
        // client's client_port off the wire, it instead relies on the test
        // below discovering the client's bound port via a side channel).
        //
        // Simpler: PCMU packets are sent to the well-known destination the
        // main test thread publishes over a channel once `RtspDemuxer::open`
        // returns. That handshake happens after this closure returns, so
        // the send happens in the test body instead — this thread's job
        // ends at PLAY.
    });

    let registry = {
        let mut r = ProtocolRegistry::new();
        vaco_protocol_socket::register(&mut r);
        r
    };
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["tcp", "udp"]);
    let opts = vaco_demux_rtsp::RtspOptions {
        min_port: 41000,
        max_port: 41100,
        ..Default::default()
    };

    let url = format!("rtsp://127.0.0.1:{}/stream", rtsp_addr.port());
    let mut demuxer = vaco_demux_rtsp::RtspDemuxer::open(
        &url,
        vaco_demux_rtsp::TransportMode::UdpUnicast,
        &opts,
        &registry,
        &env,
        &vaco_format_core::discovery::NoParsers,
    )
    .unwrap();

    server.join().unwrap();

    assert_eq!(demuxer.streams().len(), 1);

    // Build one RTP/PCMU packet and send it to the local port the demuxer
    // actually bound (recovered from the stream we just built — see
    // `RtspDemuxer::open`'s SETUP loop, which always binds starting at
    // `opts.min_port`).
    let header = vaco_format_rtp::RtpHeader {
        version: vaco_format_rtp::RTP_VERSION,
        padding: false,
        extension: false,
        marker: true,
        payload_type: 0,
        sequence_number: 1,
        timestamp: 160,
        ssrc: 0x1234_5678,
        csrc_count: 0,
    };
    let packet_bytes = vaco_format_rtp::rtp::build_basic(&header, b"pcmu-audio-bytes");
    server_rtp_socket
        .send_to(&packet_bytes, ("127.0.0.1", opts.min_port as u16))
        .unwrap();

    // Give the datagram a moment to arrive.
    thread::sleep(Duration::from_millis(50));

    let pkt = demuxer.read_packet().unwrap();
    assert_eq!(pkt.payload(), b"pcmu-audio-bytes");
    assert_eq!(pkt.stream_index, 0);
}
