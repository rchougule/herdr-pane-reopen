mod common;

use common::fixture;
use reopen::rpc::{classify_frame, parse_response, truncate, Client, Error, Frame};
use std::io::{BufRead, BufReader, Write};

fn line(key: &str) -> String {
    fixture("rpc_frames.json")[key]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn parses_a_success_envelope() {
    let r = parse_response(&line("pong")).expect("ok");
    assert_eq!(r["type"], "pong");
    assert_eq!(r["protocol"], 22);
    assert_eq!(r["version"], "0.9.1");
}

#[test]
fn parses_an_error_envelope_without_panicking() {
    let e = parse_response(&line("error_pane_not_found")).unwrap_err();
    assert_eq!(e.code(), Some("pane_not_found"));
    let e = parse_response(&line("error_agent_name_taken")).unwrap_err();
    assert_eq!(e.code(), Some("agent_name_taken"));
}

#[test]
fn malformed_output_degrades_instead_of_panicking() {
    let e = parse_response(&line("garbage")).unwrap_err();
    assert!(matches!(e, Error::Protocol(_)));
    assert_eq!(classify_frame(&line("garbage")), Frame::Unknown);
    assert!(parse_response("{\"id\":\"x\"}").is_err());
}

#[test]
fn discriminates_responses_from_events_on_the_subscription_stream() {
    // The ack carries `id`; every event afterwards is a bare {"event","data"} line.
    assert!(matches!(
        classify_frame(&line("subscription_ack")),
        Frame::Response(_)
    ));
    match classify_frame(&line("event_pane_created")) {
        Frame::Event { event, data } => {
            assert_eq!(event, "pane_created");
            assert_eq!(data["pane"]["pane_id"], "w3M:pB");
        }
        other => panic!("expected an event, got {other:?}"),
    }
    match classify_frame(&line("event_layout_updated")) {
        Frame::Event { event, .. } => assert_eq!(event, "layout_updated"),
        other => panic!("expected an event, got {other:?}"),
    }
}

#[test]
fn ndjson_round_trip_over_a_real_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let pong = line("pong");
    let server = std::thread::spawn(move || {
        // Two sequential connections: the client must open a fresh one per request.
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut req = String::new();
            r.read_line(&mut req).unwrap();
            let v: serde_json::Value = serde_json::from_str(&req).unwrap();
            assert_eq!(v["method"], "ping");
            let mut w = stream;
            w.write_all(format!("{pong}\n").as_bytes()).unwrap();
            w.flush().unwrap();
        }
    });
    let c = Client::new(&path);
    assert_eq!(c.ping().unwrap(), ("0.9.1".to_string(), 22));
    assert_eq!(c.ping().unwrap().1, 22);
    server.join().unwrap();
}

#[test]
fn a_long_multibyte_error_line_is_truncated_not_panicked() {
    // `&s[..200]` panics when byte 200 lands inside a UTF-8 sequence, and this is the
    // exact path that exists to survive a malformed payload (review F3).
    let line = format!("{{\"garbage\":\"{}\"}}", "é".repeat(400));
    assert!(matches!(parse_response(&line), Err(Error::Protocol(_))));
    // …and one where the cut lands inside a 4-byte emoji, plus a valid envelope with no
    // `result` field (the other reachable call site).
    let line = format!("{{\"id\":\"x\",\"note\":\"{}\"}}", "🙂".repeat(300));
    assert!(matches!(parse_response(&line), Err(Error::Protocol(_))));

    assert_eq!(truncate("abc", 10), "abc");
    assert_eq!(truncate("ab", 2), "ab");
    // "é" is two bytes: cutting at 3 must back off to 2, never split it
    assert_eq!(truncate("éé", 3), "é…");
    assert_eq!(truncate("🙂x", 2), "…");
}
