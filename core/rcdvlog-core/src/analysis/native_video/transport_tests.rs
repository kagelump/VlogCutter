//! Local HTTP fixtures exercise the actual client/runtime. No provider/key use.
use super::*;
use crate::{AnalysisPayload, RunPolicyParameters};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

fn prepared() -> (tempfile::TempDir, NativeVideoOptions, PreparedRequest) {
    let (dir, catalog, policy, _) =
        crate::analysis::native_video::media::media_tests::setup(AnalysisPayload::Video);
    let media = catalog
        .prepare_native_video_window(
            &policy,
            &crate::analysis::native_video::media::media_tests::window(&policy, 1.0),
            &|| false,
        )
        .unwrap();
    let RunPolicyParameters::NativeVideoReview { options } = policy.parameters else {
        unreachable!()
    };
    let wire = protocol::prepare(&options, &media).unwrap();
    (dir, options, wire)
}
fn transport() -> DirectTransport {
    let mut ready = make_client("local-fixture-key", false).unwrap();
    // A local fixture must not use a host-configured outbound proxy.
    let _enter = ready.runtime.enter();
    ready.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .unwrap();
    drop(_enter);
    DirectTransport {
        ready: Mutex::new(Some(ready)),
    }
}
fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 65536);
    }
    let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
    assert!(header.starts_with("post /fixture http/1.1\r\n"));
    assert!(header.contains("x-goog-api-key: local-fixture-key\r\n"));
    let length = header
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length:")
                .map(|s| s.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length <= protocol::MAX_BODY_BYTES);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("local-fixture-key"));
}
fn response(index: usize) -> String {
    json!({"responseId":format!("local-{index}"),"modelVersion":super::super::NATIVE_VIDEO_MODEL,
        "usageMetadata":{"serviceTier":"standard","promptTokenCount":100,"candidatesTokenCount":20,"totalTokenCount":120},
        "candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"{\"version\":1,\"clock\":\"localVideoZero\",\"moments\":[],\"flags\":[]}"}]}}]}).to_string()
}
#[test]
fn actual_http_client_reuses_live_connection_across_sequential_calls() {
    let (_dir, options, wire) = prepared();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/fixture", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        for index in 0..2 {
            read_request(&mut socket);
            let body = response(index);
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-gemini-service-tier: standard\r\nConnection: {}\r\n\r\n{}",body.len(),if index==0{"keep-alive"}else{"close"},body).unwrap();
            socket.flush().unwrap();
        }
    });
    let transport = transport();
    for _ in 0..2 {
        let start = Instant::now();
        let result = transport
            .transmit_to(&endpoint, &wire, &options, &|| {
                start.elapsed() > Duration::from_secs(5)
            })
            .unwrap();
        assert_eq!(result.status, 200);
        assert!(result.output.is_ok());
        assert!(result.receipt.usage_complete);
    }
    server.join().unwrap();
}
#[test]
fn in_flight_http_can_be_cancelled_without_claiming_no_upload() {
    let (_dir, options, wire) = prepared();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/fixture", listener.local_addr().unwrap());
    let observed = Arc::new(AtomicBool::new(false));
    let accepted = observed.clone();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        accepted.store(true, Ordering::Release);
        let _ = finish_rx.recv_timeout(Duration::from_secs(5));
    });
    let transport = transport();
    let start = Instant::now();
    let error = transport
        .transmit_to(&endpoint, &wire, &options, &|| {
            observed.load(Ordering::Acquire) || start.elapsed() > Duration::from_secs(5)
        })
        .err()
        .unwrap();
    assert!(observed.load(Ordering::Acquire));
    assert!(error.to_string().contains("after dispatch marker"));
    assert!(!error.to_string().contains("local-fixture-key"));
    assert!(start.elapsed() < Duration::from_secs(4));
    let _ = finish_tx.send(());
    server.join().unwrap();
}
