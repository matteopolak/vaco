//! #553's own Acceptance Criterion, replayed as a replacement bar: "publish
//! and play round-trip against a reference server with the same command
//! sequence the reference emits". No RTMP reference server is reachable
//! from this machine (no live `nginx-rtmp`/`srs`/Wowza to connect to), so
//! this crate follows the same replacement-bar pattern
//! `vaco-protocol-srt`/`vaco-protocol-rist` already established: name the
//! reference peer unreachable up front, then build the actual named
//! sequence (Adobe RTMP spec §7's own connect -> createStream ->
//! publish/play -> onStatus flow) end to end through this crate's real
//! chunk-stream transport, both directions, and check it against the
//! sequence the specification itself names.
//!
//! Self-consistency evidence, not a differential check: both "client" and
//! "server" here are this crate's own code driving each other through
//! [`vaco_protocol_rtmp::message::Dechunker`]/`chunk_message` — real
//! evidence the layers compose correctly, not evidence of interop with a
//! second, independent implementation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_protocol_rtmp::command::{self, Command};
use vaco_protocol_rtmp::message::{Dechunker, RtmpMessage, chunk_message};

const COMMAND_CSID: u32 = 3;
const CHUNK_SIZE: u32 = 128;

fn send(name_stream_id: u32, cmd: &Command) -> Vec<u8> {
    let msg = RtmpMessage {
        timestamp: 0,
        message_type_id: 20, // AMF0 command
        message_stream_id: name_stream_id,
        payload: cmd.encode(),
    };
    chunk_message(&msg, COMMAND_CSID, CHUNK_SIZE).expect("encoding a command message never fails")
}

fn receive_one(dechunker: &mut Dechunker, wire: &[u8]) -> Command {
    let messages = dechunker
        .feed(wire)
        .expect("well-formed chunk stream bytes decode");
    assert_eq!(
        messages.len(),
        1,
        "expected exactly one reassembled message"
    );
    Command::decode(&messages[0].payload).expect("a command message's payload decodes as a Command")
}

fn status_code(cmd: &Command) -> String {
    let vaco_protocol_rtmp::amf0::Value::Object(pairs) = cmd
        .arguments
        .first()
        .expect("status commands carry one info-object argument")
    else {
        panic!("status argument is not an AMF0 object");
    };
    for (key, value) in pairs {
        if key == "code"
            && let vaco_protocol_rtmp::amf0::Value::String(s) = value
        {
            return s.clone();
        }
    }
    panic!("info object has no \"code\" key");
}

/// The full `connect` -> `createStream` -> `publish` -> `onStatus` sequence,
/// Adobe RTMP spec §7's own named commands in the order it documents them.
#[test]
fn publish_flow_emits_the_specs_own_command_sequence() {
    let mut server_side = Dechunker::new(vaco_limits::Limits::strict());
    let mut client_side = Dechunker::new(vaco_limits::Limits::strict());

    // 1. connect
    let connect_cmd = command::connect("live", "rtmp://vaco.test/live");
    let on_server = receive_one(&mut server_side, &send(0, &connect_cmd));
    assert_eq!(on_server.name, "connect");

    let result = command::connect_result(connect_cmd.transaction_id);
    let on_client = receive_one(&mut client_side, &send(0, &result));
    assert_eq!(on_client.name, "_result");
    assert_eq!(status_code(&on_client), "NetConnection.Connect.Success");

    // 2. createStream
    let create_cmd = command::create_stream(2.0);
    let on_server = receive_one(&mut server_side, &send(0, &create_cmd));
    assert_eq!(on_server.name, "createStream");

    let stream_id = 1.0;
    let result = command::create_stream_result(create_cmd.transaction_id, stream_id);
    let on_client = receive_one(&mut client_side, &send(0, &result));
    assert_eq!(on_client.name, "_result");
    let vaco_protocol_rtmp::amf0::Value::Number(returned_id) = on_client
        .arguments
        .first()
        .expect("createStream _result carries the new stream ID")
    else {
        panic!("createStream _result argument is not a Number");
    };
    assert_eq!(*returned_id, stream_id);

    // 3. publish, on the stream ID createStream returned
    let publish_cmd = command::publish("mystream", "live");
    let on_server = receive_one(&mut server_side, &send(1, &publish_cmd));
    assert_eq!(on_server.name, "publish");

    let status = command::on_status_publish_start("mystream");
    let on_client = receive_one(&mut client_side, &send(1, &status));
    assert_eq!(on_client.name, "onStatus");
    assert_eq!(status_code(&on_client), "NetStream.Publish.Start");
}

/// The `connect` -> `createStream` -> `play` -> `onStatus` sequence.
#[test]
fn play_flow_emits_the_specs_own_command_sequence() {
    let mut server_side = Dechunker::new(vaco_limits::Limits::strict());
    let mut client_side = Dechunker::new(vaco_limits::Limits::strict());

    let connect_cmd = command::connect("live", "rtmp://vaco.test/live");
    receive_one(&mut server_side, &send(0, &connect_cmd));
    receive_one(&mut client_side, &send(0, &command::connect_result(1.0)));

    let create_cmd = command::create_stream(2.0);
    receive_one(&mut server_side, &send(0, &create_cmd));
    receive_one(
        &mut client_side,
        &send(0, &command::create_stream_result(2.0, 1.0)),
    );

    let play_cmd = command::play("mystream");
    let on_server = receive_one(&mut server_side, &send(1, &play_cmd));
    assert_eq!(on_server.name, "play");

    let status = command::on_status_play_start("mystream");
    let on_client = receive_one(&mut client_side, &send(1, &status));
    assert_eq!(on_client.name, "onStatus");
    assert_eq!(status_code(&on_client), "NetStream.Play.Start");
}
